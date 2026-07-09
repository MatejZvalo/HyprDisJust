use ratatui::layout::Rect;

use crate::profile::store::ProfileOutput;

pub const TERMINAL_CELL_ASPECT_RATIO: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapDirection {
    Left,
    Right,
    Above,
    Below,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl LogicalRect {
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasTransform {
    bounds: LogicalRect,
    area: Rect,
    cell_width: f64,
    cell_height: f64,
}

impl CanvasTransform {
    pub fn new(outputs: &[ProfileOutput], area: Rect) -> Self {
        let mut bounds = layout_bounds(outputs).unwrap_or(LogicalRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        });
        let usable_width = u16::max(area.width, 1);
        let usable_height = u16::max(area.height, 1);
        let cell_width = f64::max(
            f64::from(bounds.width.max(1)) / f64::from(usable_width),
            f64::from(bounds.height.max(1))
                / (f64::from(usable_height) * TERMINAL_CELL_ASPECT_RATIO),
        )
        .max(1.0);
        let cell_height = cell_width * TERMINAL_CELL_ASPECT_RATIO;

        bounds.width = (f64::from(usable_width) * cell_width).round() as i32;
        bounds.height = (f64::from(usable_height) * cell_height).round() as i32;

        Self {
            bounds,
            area,
            cell_width,
            cell_height,
        }
    }

    pub fn to_cell_rect(self, rect: LogicalRect) -> Rect {
        if self.area.width == 0 || self.area.height == 0 {
            return Rect {
                x: self.area.x,
                y: self.area.y,
                width: 0,
                height: 0,
            };
        }

        let x = self.area.x + scaled_offset(rect.x - self.bounds.x, self.cell_width);
        let y = self.area.y + scaled_offset(rect.y - self.bounds.y, self.cell_height);
        let width = scaled_size(rect.width, self.cell_width, self.area.width);
        let height = scaled_size(rect.height, self.cell_height, self.area.height);
        let (x, width) = clamp_span_to_area(x, width, self.area.x, self.area.width);
        let (y, height) = clamp_span_to_area(y, height, self.area.y, self.area.height);

        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn to_logical(self, column: u16, row: u16) -> Option<(i32, i32)> {
        if column < self.area.x
            || row < self.area.y
            || column >= self.area.right()
            || row >= self.area.bottom()
        {
            return None;
        }

        let x = self.bounds.x + ((column - self.area.x) as f64 * self.cell_width).round() as i32;
        let y = self.bounds.y + ((row - self.area.y) as f64 * self.cell_height).round() as i32;
        Some((x, y))
    }

    pub fn cell_delta_to_logical(self, dx: i32, dy: i32) -> (i32, i32) {
        (
            (f64::from(dx) * self.cell_width).round() as i32,
            (f64::from(dy) * self.cell_height).round() as i32,
        )
    }

    pub fn output_at(self, outputs: &[ProfileOutput], column: u16, row: u16) -> Option<usize> {
        let (x, y) = self.to_logical(column, row)?;
        outputs
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, output)| {
                output_rect(output)
                    .filter(|rect| rect.contains(x, y))
                    .map(|_| index)
            })
    }
}

pub fn output_rect(output: &ProfileOutput) -> Option<LogicalRect> {
    if !output.enabled {
        return None;
    }

    let (mut width, mut height) = parse_mode_dimensions(&output.mode)?;
    if matches!(output.transform, 1 | 3 | 5 | 7) {
        std::mem::swap(&mut width, &mut height);
    }

    if width <= 0 || height <= 0 || output.scale <= 0.0 || !output.scale.is_finite() {
        return None;
    }

    Some(LogicalRect {
        x: output.x,
        y: output.y,
        width: (f64::from(width) / output.scale).round().max(1.0) as i32,
        height: (f64::from(height) / output.scale).round().max(1.0) as i32,
    })
}

pub fn move_output(output: &mut ProfileOutput, dx: i32, dy: i32) {
    if output.enabled {
        output.x = output.x.saturating_add(dx);
        output.y = output.y.saturating_add(dy);
    }
}

pub fn snap_output(
    outputs: &mut [ProfileOutput],
    selected_index: usize,
    target_index: usize,
    direction: SnapDirection,
) -> bool {
    if selected_index == target_index {
        return false;
    }

    let Some(selected_rect) = outputs.get(selected_index).and_then(output_rect) else {
        return false;
    };
    let Some(target_rect) = outputs.get(target_index).and_then(output_rect) else {
        return false;
    };

    let selected = &mut outputs[selected_index];
    match direction {
        SnapDirection::Left => {
            selected.x = target_rect.x - selected_rect.width;
            selected.y = target_rect.y;
        }
        SnapDirection::Right => {
            selected.x = target_rect.x + target_rect.width;
            selected.y = target_rect.y;
        }
        SnapDirection::Above => {
            selected.x = target_rect.x;
            selected.y = target_rect.y - selected_rect.height;
        }
        SnapDirection::Below => {
            selected.x = target_rect.x;
            selected.y = target_rect.y + target_rect.height;
        }
    }

    true
}

fn layout_bounds(outputs: &[ProfileOutput]) -> Option<LogicalRect> {
    let rects: Vec<_> = outputs.iter().filter_map(output_rect).collect();
    let first = rects.first()?;
    let mut left = first.x.min(0);
    let mut top = first.y.min(0);
    let mut total_width = 0;
    let mut total_height = 0;
    let mut widest = 0;
    let mut tallest = 0;

    for rect in &rects {
        left = left.min(rect.x);
        top = top.min(rect.y);
        total_width += rect.width;
        total_height += rect.height;
        widest = widest.max(rect.width);
        tallest = tallest.max(rect.height);
    }

    let logical_width = total_width.max(widest).max(1);
    let logical_height = total_height.max(tallest).max(1);
    let padding_x = (logical_width / 20).max(80);
    let padding_y = (logical_height / 20).max(80);

    Some(LogicalRect {
        x: left - padding_x,
        y: top - padding_y,
        width: (logical_width + padding_x * 2).max(1),
        height: (logical_height + padding_y * 2).max(1),
    })
}

fn parse_mode_dimensions(mode: &str) -> Option<(i32, i32)> {
    let dimensions = mode
        .split_once('@')
        .map_or(mode, |(dimensions, _)| dimensions);
    let (width, height) = dimensions.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn scaled_offset(value: i32, cell_size: f64) -> u16 {
    (f64::from(value) / cell_size).round().max(0.0) as u16
}

fn scaled_size(value: i32, cell_size: f64, max_size: u16) -> u16 {
    let size = (f64::from(value) / cell_size).round().max(1.0) as u16;
    size.min(max_size.max(1))
}

fn clamp_span_to_area(start: u16, size: u16, area_start: u16, area_size: u16) -> (u16, u16) {
    if area_size == 0 {
        return (area_start, 0);
    }

    if size >= area_size {
        return (area_start, area_size);
    }

    let area_end = area_start.saturating_add(area_size);
    let max_start = area_end.saturating_sub(size);
    (start.clamp(area_start, max_start), size)
}
