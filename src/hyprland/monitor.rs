#[derive(Debug, Clone, serde::Deserialize)]
pub struct RawHyprMonitor {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub make: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub serial: String,
    pub width: i32,
    pub height: i32,
    #[serde(default, rename = "physicalWidth")]
    pub physical_width: i32,
    #[serde(default, rename = "physicalHeight")]
    pub physical_height: i32,
    #[serde(default, rename = "refreshRate")]
    pub refresh_rate: f64,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub scale: f64,
    #[serde(default)]
    pub transform: i32,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, rename = "availableModes")]
    pub available_modes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorState {
    pub output_name: String,
    pub id: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub enabled: bool,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub refresh_rate: f64,
    pub scale: f64,
    pub transform: i32,
    pub available_modes: Vec<String>,
    pub physical_width: i32,
    pub physical_height: i32,
}

pub fn parse_raw_monitors(json: &str) -> anyhow::Result<Vec<RawHyprMonitor>> {
    Ok(serde_json::from_str(json)?)
}

impl From<RawHyprMonitor> for MonitorState {
    fn from(raw: RawHyprMonitor) -> Self {
        let id = stable_monitor_id(
            &raw.make,
            &raw.model,
            &raw.serial,
            &raw.description,
            &raw.name,
        );

        Self {
            output_name: raw.name,
            id,
            description: raw.description,
            make: raw.make,
            model: raw.model,
            serial: raw.serial,
            enabled: !raw.disabled,
            x: raw.x,
            y: raw.y,
            width: raw.width,
            height: raw.height,
            refresh_rate: raw.refresh_rate,
            scale: raw.scale,
            transform: raw.transform,
            available_modes: raw.available_modes,
            physical_width: raw.physical_width,
            physical_height: raw.physical_height,
        }
    }
}

pub fn stable_monitor_id(
    make: &str,
    model: &str,
    serial: &str,
    description: &str,
    output_name: &str,
) -> String {
    let make = make.trim();
    let model = model.trim();
    let serial = serial.trim();
    let description = description.trim();

    if useful(make) && useful(model) && useful(serial) {
        return format!(
            "{}:{}:{}",
            slug_component(make),
            slug_component(model),
            slug_component(serial)
        );
    }

    if useful(make) && useful(model) {
        return format!(
            "{}:{}:no-serial",
            slug_component(make),
            slug_component(model)
        );
    }

    if useful(description) {
        return format!("description:{}", slug_component(description));
    }

    format!("output:{}", slug_component(output_name))
}

pub fn slug_component(input: &str) -> String {
    let mut slug = String::new();
    let mut previous_was_dash = false;

    for character in input.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            previous_was_dash = false;
        } else if !previous_was_dash && !slug.is_empty() {
            slug.push('-');
            previous_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "unknown".to_owned()
    } else {
        slug.to_owned()
    }
}

fn useful(value: &str) -> bool {
    !value.trim().is_empty() && slug_component(value) != "unknown"
}
