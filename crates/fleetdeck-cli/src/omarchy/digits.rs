//! The digit table of the ambient screensaver (R13 P4 task G,
//! `omarchy-p4.md` §3.8): digits 0 to 9, 5 rows of 5 columns, drawn with `#`.

/// The rows of each digit. Every row has exactly 5 characters.
pub const GLYPHS: [[&str; 5]; 10] = [
    ["#####", "#   #", "#   #", "#   #", "#####"],
    ["  #  ", " ##  ", "  #  ", "  #  ", "#####"],
    ["#####", "    #", "#####", "#    ", "#####"],
    ["#####", "    #", "#####", "    #", "#####"],
    ["#   #", "#   #", "#####", "    #", "    #"],
    ["#####", "#    ", "#####", "    #", "#####"],
    ["#####", "#    ", "#####", "#   #", "#####"],
    ["#####", "    #", "   # ", "  #  ", "  #  "],
    ["#####", "#   #", "#####", "#   #", "#####"],
    ["#####", "#   #", "#####", "    #", "#####"],
];

/// The largest number that the table shows.
pub const MAX: u32 = 99;

/// The 5 rows of `n`, with one blank column between two digits. A number
/// above [`MAX`] shows as `99`.
#[must_use]
pub fn rows(n: u32) -> [String; 5] {
    let n = n.min(MAX);
    let digits: Vec<usize> = n
        .to_string()
        .bytes()
        .map(|b| usize::from(b - b'0'))
        .collect();
    std::array::from_fn(|row| {
        digits
            .iter()
            .map(|&d| GLYPHS[d][row])
            .collect::<Vec<_>>()
            .join(" ")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_every_glyph_is_5_rows_of_5_columns_of_hash_or_space() {
        for (digit, glyph) in GLYPHS.iter().enumerate() {
            for row in glyph {
                assert_eq!(row.len(), 5, "digit {digit}");
                assert!(row.bytes().all(|b| b == b'#' || b == b' '), "digit {digit}");
            }
        }
    }

    #[test]
    fn digits_the_rows_for_0_to_9_are_the_table() {
        for n in 0..=9_u32 {
            let got = rows(n);
            for (row, text) in got.iter().enumerate() {
                assert_eq!(text, GLYPHS[n as usize][row], "digit {n} row {row}");
            }
        }
        assert_eq!(
            rows(7),
            ["#####", "    #", "   # ", "  #  ", "  #  "].map(String::from)
        );
        assert_eq!(
            rows(0),
            ["#####", "#   #", "#   #", "#   #", "#####"].map(String::from)
        );
    }

    #[test]
    fn digits_two_digits_have_one_blank_column_between_them() {
        let got = rows(10);
        assert_eq!(got[0], "  #   #####");
        assert_eq!(got[4], "##### #####");
        assert!(got.iter().all(|r| r.len() == 11));
    }

    #[test]
    fn digits_a_number_above_99_shows_99() {
        assert_eq!(rows(100), rows(99));
        assert_eq!(rows(u32::MAX), rows(99));
        assert_ne!(rows(99), rows(9));
    }
}
