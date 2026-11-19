use hfdx_core::display::{bytes, speed};

#[test]
fn decimal_units_round_and_carry_without_overflow() {
    for (value, expected) in [
        (0, "0 B"),
        (999, "999 B"),
        (1_000, "1 KB"),
        (1_005, "1.01 KB"),
        (32_500, "32.5 KB"),
        (999_994, "999.99 KB"),
        (999_995, "1 MB"),
        (999_999, "1 MB"),
        (1_000_000, "1 MB"),
        (999_999_999, "1 GB"),
        (1_200_000_000, "1.2 GB"),
        (1_000_000_000_000, "1 TB"),
        (u64::MAX, "18446744.07 TB"),
    ] {
        assert_eq!(bytes(value), expected, "{value}");
    }
}

#[test]
fn speed_distinguishes_unknown_from_zero() {
    assert_eq!(speed(None), "—");
    assert_eq!(speed(Some(0)), "0 B/s");
    assert_eq!(speed(Some(32_500)), "32.5 KB/s");
}
