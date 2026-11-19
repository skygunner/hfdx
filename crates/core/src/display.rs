pub fn bytes(value: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let value = u128::from(value);
    let mut unit = 0;
    let mut scale = 1_u128;
    while unit < units.len() - 1 && value >= scale * 1_000 {
        unit += 1;
        scale *= 1_000;
    }
    let mut hundredths = (value * 100 + scale / 2) / scale;
    if hundredths >= 100_000 && unit < units.len() - 1 {
        unit += 1;
        scale *= 1_000;
        hundredths = (value * 100 + scale / 2) / scale;
    }
    let whole = hundredths / 100;
    let fraction = hundredths % 100;
    if fraction == 0 {
        format!("{whole} {}", units[unit])
    } else if fraction.is_multiple_of(10) {
        format!("{whole}.{} {}", fraction / 10, units[unit])
    } else {
        format!("{whole}.{fraction:02} {}", units[unit])
    }
}

pub fn speed(value: Option<u64>) -> String {
    match value {
        Some(value) => format!("{}/s", bytes(value)),
        None => "—".to_owned(),
    }
}
