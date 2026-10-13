//! Bitsets over node indices, stored as `Vec<u64>`. Reading past the end gives `false`; writing
//! past the end grows the vector.

pub(crate) fn get(v: &[u64], i: usize) -> bool {
    v.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
}

pub(crate) fn set(v: &mut Vec<u64>, i: usize, on: bool) {
    let (w, b) = (i / 64, i % 64);
    if w >= v.len() {
        v.resize(w + 1, 0);
    }
    if on {
        v[w] |= 1 << b;
    } else {
        v[w] &= !(1 << b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_set_and_growth() {
        let mut v = Vec::new();
        assert!(!get(&v, 1000));
        set(&mut v, 130, true);
        assert!(get(&v, 130) && !get(&v, 129) && !get(&v, 131));
        set(&mut v, 130, false);
        assert!(!get(&v, 130));
        set(&mut v, 5000, false);
        assert!(!get(&v, 5000));
    }
}
