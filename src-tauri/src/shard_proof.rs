pub fn proof_score(a: u32, b: u32) -> u32 {
    if a > b {
        a - b
    } else {
        b * 2 + a
    }
}

#[cfg(test)]
mod tests {
    use super::proof_score;

    #[test]
    fn scores_both_branches() {
        assert_eq!(proof_score(5, 2), 3);
        assert_eq!(proof_score(2, 5), 12);
    }
}
