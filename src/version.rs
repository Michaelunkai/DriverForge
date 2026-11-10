//! Version parsing/comparison. Never panics.
use std::cmp::Ordering;

#[derive(Debug, PartialEq, Eq)]
struct Parsed {
    nums: Vec<String>,   // digit strings, leading zeros stripped
    pre: Option<Vec<String>>, // Some(numbers after a pre-release word)
}

fn strip_zeros(s: &str) -> String {
    let t = s.trim_start_matches('0');
    if t.is_empty() { "0".into() } else { t.into() }
}

fn cmp_num(a: &str, b: &str) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn cmp_vec(a: &[String], b: &[String]) -> Ordering {
    let n = a.len().max(b.len());
    let zero = "0".to_string();
    for i in 0..n {
        let x = a.get(i).unwrap_or(&zero);
        let y = b.get(i).unwrap_or(&zero);
        let o = cmp_num(x, y);
        if o != Ordering::Equal { return o; }
    }
    Ordering::Equal
}

fn is_pre_word(w: &str) -> bool {
    matches!(w, "alpha" | "beta" | "rc" | "pre" | "preview" | "dev" | "test" | "nightly" | "snapshot" | "canary")
}

fn parse(v: &str) -> Parsed {
    let mut s = v.trim();
    if let Some(r) = s.strip_prefix(['v', 'V']) {
        if r.starts_with(|c: char| c.is_ascii_digit()) { s = r; }
    }
    // tokenize into digit runs and letter runs (ASCII only; others are separators)
    let mut toks: Vec<(bool, String)> = Vec::new();
    for c in s.chars() {
        let kind = if c.is_ascii_digit() { Some(true) } else if c.is_ascii_alphabetic() { Some(false) } else { None };
        match kind {
            None => toks.push((true, String::new())), // separator marker
            Some(k) => match toks.last_mut() {
                Some((lk, t)) if *lk == k && !t.is_empty() => t.push(c.to_ascii_lowercase()),
                _ => toks.push((k, c.to_ascii_lowercase().to_string())),
            },
        }
    }
    let toks: Vec<(bool, String)> = toks.into_iter().filter(|(_, t)| !t.is_empty()).collect();
    let mut nums = Vec::new();
    let mut i = 0;
    while i < toks.len() && toks[i].0 {
        nums.push(strip_zeros(&toks[i].1));
        i += 1;
    }
    let mut pre = None;
    if i < toks.len() && is_pre_word(&toks[i].1) {
        let tail: Vec<String> = toks[i + 1..].iter().filter(|(d, _)| *d).map(|(_, t)| strip_zeros(t)).collect();
        pre = Some(tail);
    }
    Parsed { nums, pre }
}

pub fn cmp(a: &str, b: &str) -> Ordering {
    let (pa, pb) = (parse(a), parse(b));
    cmp_vec(&pa.nums, &pb.nums).then_with(|| match (&pa.pre, &pb.pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => cmp_vec(x, y),
    })
}

pub fn is_newer(latest: &str, installed: &str) -> bool {
    cmp(latest, installed) == Ordering::Greater
}

/// Windows driver version 32.0.16.2114 -> NVIDIA version 621.14.
pub fn nvidia_from_windows(v: &str) -> Option<String> {
    let parts: Vec<&str> = v.trim().split('.').collect();
    if parts.len() != 4 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let last = parts[2].chars().last()?;
    let p4 = parts[3];
    if p4.len() > 4 { return None; }
    let digits = format!("{last}{p4:0>4}");
    let (maj, min) = digits.split_at(3);
    let maj = maj.trim_start_matches('0');
    if maj.is_empty() { return None; }
    Some(format!("{maj}.{min}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    #[test] fn nv_basic() { assert_eq!(nvidia_from_windows("32.0.16.2114").as_deref(), Some("621.14")); }
    #[test] fn nv_560() { assert_eq!(nvidia_from_windows("32.0.15.6094").as_deref(), Some("560.94")); }
    #[test] fn nv_ws() { assert_eq!(nvidia_from_windows(" 31.0.15.5123 ").as_deref(), Some("551.23")); }
    #[test] fn nv_short() { assert_eq!(nvidia_from_windows("32.0.16"), None); }
    #[test] fn nv_long() { assert_eq!(nvidia_from_windows("32.0.16.2114.1"), None); }
    #[test] fn nv_empty() { assert_eq!(nvidia_from_windows(""), None); }
    #[test] fn nv_alpha() { assert_eq!(nvidia_from_windows("32.0.x.2114"), None); }
    #[test] fn nv_empty_part() { assert_eq!(nvidia_from_windows("32.0..2114"), None); }
    #[test] fn nv_p4_long() { assert_eq!(nvidia_from_windows("32.0.16.21145"), None); }
    #[test] fn nv_unicode() { assert_eq!(nvidia_from_windows("３２.0.16.2114"), None); }
    #[test] fn nv_padded() { assert_eq!(nvidia_from_windows("32.0.15.0994").as_deref(), Some("509.94")); }
    #[test] fn nv_leading_zero_major() { assert_eq!(nvidia_from_windows("32.0.10.0094"), None); }

    #[test] fn eq_trailing_zero() { assert_eq!(cmp("1.0", "1.0.0"), Equal); }
    #[test] fn unequal_len() { assert_eq!(cmp("1.2.3", "1.2"), Greater); }
    #[test] fn numeric_not_lex() { assert_eq!(cmp("1.10", "1.9"), Greater); }
    #[test] fn leading_zeros() { assert_eq!(cmp("1.02", "1.2"), Equal); }
    #[test] fn v_prefix() { assert_eq!(cmp("v1.2", "1.2"), Equal); }
    #[test] fn v_upper() { assert_eq!(cmp("V2.0", "1.9"), Greater); }
    #[test] fn whql_ignored() { assert_eq!(cmp("621.14 WHQL", "621.14"), Equal); }
    #[test] fn beta_less() { assert_eq!(cmp("1.2.3-beta", "1.2.3"), Less); }
    #[test] fn beta_numbers() { assert_eq!(cmp("1.2-beta2", "1.2-beta10"), Less); }
    #[test] fn rc_vs_beta_same_rank() { assert_eq!(cmp("1.0-rc1", "1.0-rc1"), Equal); }
    #[test] fn beta_newer_base() { assert_eq!(cmp("1.3-beta", "1.2"), Greater); }
    #[test] fn dates() { assert_eq!(cmp("2024-05-01", "2023-12-31"), Greater); }
    #[test] fn dates_dotted() { assert_eq!(cmp("2024.05.01", "2024.5.2"), Less); }
    #[test] fn big_numbers() { assert_eq!(cmp("99999999999999999999999.1", "99999999999999999999998.9"), Greater); }
    #[test] fn amd_ver() { assert_eq!(cmp("32.0.21045.20006", "32.0.21045.20007"), Less); }
    #[test] fn amd_chipset() { assert_eq!(cmp("8.09.28.1124", "7.99.1"), Greater); }
    #[test] fn empty_both() { assert_eq!(cmp("", ""), Equal); }
    #[test] fn empty_vs_ver() { assert_eq!(cmp("", "1.0"), Less); }
    #[test] fn garbage() { assert_eq!(cmp("abc", "xyz"), Equal); }
    #[test] fn unicode_no_panic() { let _ = cmp("１.２ ü", "é"); }
    #[test] fn commas_underscores() { assert_eq!(cmp("1_2_3", "1.2.3"), Equal); }
    #[test] fn digits_letters_glued() { assert_eq!(cmp("1.2.3a", "1.2.3"), Equal); }
    #[test] fn is_newer_true() { assert!(is_newer("621.14", "560.94")); }
    #[test] fn is_newer_equal_false() { assert!(!is_newer("1.0", "1.0.0")); }
    #[test] fn is_newer_older_false() { assert!(!is_newer("1.0", "1.1")); }
    #[test] fn antisymmetric() {
        for (a, b) in [("1.2", "1.10"), ("2-beta", "2"), ("", "0")] {
            assert_eq!(cmp(a, b), cmp(b, a).reverse());
        }
    }
}
