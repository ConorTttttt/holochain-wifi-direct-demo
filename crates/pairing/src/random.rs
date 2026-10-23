//! Random values from the operating system's CSPRNG.

/// Fill a fixed-size array from the OS CSPRNG.
pub fn bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("the OS random number generator failed");
    out
}

/// A random string of `len` characters drawn uniformly from `alphabet`.
///
/// Uses rejection sampling, so every character is equally likely.
pub fn string(alphabet: &[u8], len: usize) -> String {
    assert!(!alphabet.is_empty() && alphabet.len() <= 256);
    // The largest multiple of the alphabet size that fits in a byte.
    let limit = 256 - (256 % alphabet.len());
    let mut out = String::with_capacity(len);
    while out.len() < len {
        for byte in bytes::<64>() {
            if (byte as usize) < limit {
                out.push(alphabet[byte as usize % alphabet.len()] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    out
}

/// Letters and digits, which every Wi-Fi stack and keyboard handles.
pub const ALPHANUMERIC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_has_the_length_and_alphabet_asked_for() {
        let s = string(b"ab", 100);
        assert_eq!(s.len(), 100);
        assert!(s.bytes().all(|b| b == b'a' || b == b'b'));
    }

    #[test]
    fn bytes_differ_between_calls() {
        assert_ne!(bytes::<32>(), bytes::<32>());
    }
}
