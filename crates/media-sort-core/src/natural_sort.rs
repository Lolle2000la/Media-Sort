use std::cmp::Ordering;

/// Natural human sort order so `page2.jpg` comes before `page10.jpg`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = 0usize;
    let mut bi = 0usize;
    let ab = a.as_bytes();
    let bb = b.as_bytes();

    while ai < ab.len() && bi < bb.len() {
        let ac = ab[ai];
        let bc = bb[bi];

        if ac.is_ascii_digit() && bc.is_ascii_digit() {
            let mut a_end = ai;
            while a_end < ab.len() && ab[a_end].is_ascii_digit() {
                a_end += 1;
            }
            let mut b_end = bi;
            while b_end < bb.len() && bb[b_end].is_ascii_digit() {
                b_end += 1;
            }
            let a_digits = trim_leading_zeros(&ab[ai..a_end]);
            let b_digits = trim_leading_zeros(&bb[bi..b_end]);

            match a_digits.len().cmp(&b_digits.len()) {
                Ordering::Equal => match a_digits.cmp(b_digits) {
                    Ordering::Equal => {
                        // Prefer fewer leading zeros when values tie (`2` before `02`).
                        let len_cmp = (a_end - ai).cmp(&(b_end - bi));
                        if len_cmp != Ordering::Equal {
                            return len_cmp;
                        }
                    }
                    ord => return ord,
                },
                ord => return ord,
            }
            ai = a_end;
            bi = b_end;
        } else {
            let al = ac.to_ascii_lowercase();
            let bl = bc.to_ascii_lowercase();
            match al.cmp(&bl) {
                Ordering::Equal => {
                    ai += 1;
                    bi += 1;
                }
                ord => return ord,
            }
        }
    }

    ai.cmp(&bi)
        .then_with(|| ab.len().cmp(&bb.len()))
        .then_with(|| a.cmp(b))
}

fn trim_leading_zeros(bytes: &[u8]) -> &[u8] {
    let first_non_zero = bytes.iter().position(|&b| b != b'0').unwrap_or(bytes.len());
    &bytes[first_non_zero..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_numeric_suffixes_naturally() {
        let mut names = vec!["page10.jpg", "page2.jpg", "page1.jpg", "page20.jpg"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            vec!["page1.jpg", "page2.jpg", "page10.jpg", "page20.jpg"]
        );
    }

    #[test]
    fn prefers_fewer_leading_zeros_on_numeric_tie() {
        assert_eq!(natural_cmp("2", "02"), Ordering::Less);
        assert_eq!(natural_cmp("02", "2"), Ordering::Greater);
        assert_eq!(natural_cmp("page2.png", "page02.png"), Ordering::Less);
        assert_eq!(natural_cmp("00", "000"), Ordering::Less);
    }

    #[test]
    fn compares_ascii_letters_case_insensitively() {
        assert_eq!(natural_cmp("Chapter1", "chapter2"), Ordering::Less);
        assert_eq!(natural_cmp("chapter10", "Chapter2"), Ordering::Greater);
    }

    #[test]
    fn orders_shorter_prefix_first() {
        assert_eq!(natural_cmp("page", "page1"), Ordering::Less);
        assert_eq!(natural_cmp("page1", "page"), Ordering::Greater);
        assert_eq!(natural_cmp("", "a"), Ordering::Less);
    }

    #[test]
    fn sorts_multi_segment_version_strings() {
        let mut versions = vec![
            "v1.10.2", "v1.2.10", "v1.2.2", "v1.10.1", "v2.0.0", "v1.2.1",
        ];
        versions.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            versions,
            vec![
                "v1.2.1", "v1.2.2", "v1.2.10", "v1.10.1", "v1.10.2", "v2.0.0",
            ]
        );
    }

    #[test]
    fn breaks_exact_case_folded_ties_by_byte_order() {
        assert_eq!(natural_cmp("Page1.jpg", "page1.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("page1.jpg", "Page1.jpg"), Ordering::Greater);
        assert_eq!(natural_cmp("page1.jpg", "page1.jpg"), Ordering::Equal);
    }
}
