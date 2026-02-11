//! Telnet IAC (Interpret As Command) sequence stripping.
//!
//! DX cluster nodes use the telnet protocol. We need to strip IAC control
//! sequences from the data stream and refuse all option negotiations.

/// Telnet protocol constants.
const IAC: u8 = 255;
const WILL: u8 = 251;
const WONT: u8 = 252;
const DO: u8 = 253;
const DONT: u8 = 254;
const SB: u8 = 250;
const SE: u8 = 240;

/// Result of IAC stripping.
pub struct IacStripResult {
    /// IAC WONT responses to send back for each IAC DO received.
    pub responses: Vec<[u8; 3]>,
}

/// Strip telnet IAC sequences from a byte buffer in-place.
///
/// - Removes IAC DO/DONT/WILL/WONT sequences (3 bytes each)
/// - Removes IAC SB ... IAC SE subnegotiation sequences
/// - Converts IAC IAC (escaped 0xFF) to a single 0xFF byte
/// - Collects IAC WONT responses for all IAC DO requests received
pub fn strip_iac(buf: &mut Vec<u8>) -> IacStripResult {
    let mut result = IacStripResult {
        responses: Vec::new(),
    };

    if buf.is_empty() {
        return result;
    }

    let mut read = 0;
    let mut write = 0;
    let len = buf.len();

    while read < len {
        if buf[read] != IAC {
            buf[write] = buf[read];
            write += 1;
            read += 1;
            continue;
        }

        // IAC at end of buffer: keep it (might be split across reads)
        if read + 1 >= len {
            buf[write] = buf[read];
            write += 1;
            break;
        }

        match buf[read + 1] {
            IAC => {
                // IAC IAC -> literal 0xFF
                buf[write] = 0xFF;
                write += 1;
                read += 2;
            }
            DO => {
                // IAC DO option -> respond with IAC WONT option
                if read + 2 < len {
                    let option = buf[read + 2];
                    result.responses.push([IAC, WONT, option]);
                    read += 3;
                } else {
                    // Incomplete sequence at end; drop it
                    read = len;
                }
            }
            DONT | WILL | WONT => {
                // IAC DONT/WILL/WONT option -> strip (3 bytes)
                if read + 2 < len {
                    read += 3;
                } else {
                    read = len;
                }
            }
            SB => {
                // IAC SB ... IAC SE -> skip entire subnegotiation
                read += 2;
                while read < len {
                    if buf[read] == IAC && read + 1 < len && buf[read + 1] == SE {
                        read += 2;
                        break;
                    }
                    read += 1;
                }
            }
            _ => {
                // Unknown IAC command: strip the 2-byte sequence
                read += 2;
            }
        }
    }

    buf.truncate(write);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_iac_unchanged() {
        let mut buf = b"Hello, World!".to_vec();
        let result = strip_iac(&mut buf);
        assert_eq!(buf, b"Hello, World!");
        assert!(result.responses.is_empty());
    }

    #[test]
    fn iac_do_stripped_and_response_generated() {
        // IAC DO ECHO (255 253 1)
        let mut buf = vec![b'A', IAC, DO, 1, b'B'];
        let result = strip_iac(&mut buf);
        assert_eq!(buf, b"AB");
        assert_eq!(result.responses.len(), 1);
        assert_eq!(result.responses[0], [IAC, WONT, 1]);
    }

    #[test]
    fn iac_will_stripped() {
        let mut buf = vec![b'A', IAC, WILL, 3, b'B'];
        let result = strip_iac(&mut buf);
        assert_eq!(buf, b"AB");
        assert!(result.responses.is_empty());
    }

    #[test]
    fn iac_wont_stripped() {
        let mut buf = vec![IAC, WONT, 1, b'X'];
        let result = strip_iac(&mut buf);
        assert_eq!(buf, b"X");
        assert!(result.responses.is_empty());
    }

    #[test]
    fn iac_dont_stripped() {
        let mut buf = vec![IAC, DONT, 5];
        let result = strip_iac(&mut buf);
        assert!(buf.is_empty());
        assert!(result.responses.is_empty());
    }

    #[test]
    fn iac_iac_becomes_literal_ff() {
        let mut buf = vec![b'A', IAC, IAC, b'B'];
        strip_iac(&mut buf);
        assert_eq!(buf, vec![b'A', 0xFF, b'B']);
    }

    #[test]
    fn iac_subnegotiation_stripped() {
        // IAC SB 24 ... IAC SE
        let mut buf = vec![b'X', IAC, SB, 24, 0, b'V', b'T', IAC, SE, b'Y'];
        strip_iac(&mut buf);
        assert_eq!(buf, b"XY");
    }

    #[test]
    fn multiple_iac_sequences() {
        let mut buf = vec![IAC, DO, 1, IAC, DO, 3, b'H', b'i', IAC, WILL, 5];
        let result = strip_iac(&mut buf);
        assert_eq!(buf, b"Hi");
        assert_eq!(result.responses.len(), 2);
    }

    #[test]
    fn empty_buffer() {
        let mut buf = Vec::new();
        let result = strip_iac(&mut buf);
        assert!(buf.is_empty());
        assert!(result.responses.is_empty());
    }

    #[test]
    fn iac_at_end_of_buffer() {
        let mut buf = vec![b'A', IAC];
        strip_iac(&mut buf);
        assert_eq!(buf, vec![b'A', IAC]);
    }

    #[test]
    fn realistic_cluster_login() {
        // Simulate: IAC DO ECHO, IAC WILL SGA, then "login: "
        let mut buf = vec![
            IAC, DO, 1, // DO ECHO
            IAC, WILL, 3, // WILL SGA
            b'l', b'o', b'g', b'i', b'n', b':', b' ',
        ];
        let result = strip_iac(&mut buf);
        assert_eq!(buf, b"login: ");
        assert_eq!(result.responses.len(), 1);
        assert_eq!(result.responses[0], [IAC, WONT, 1]);
    }
}
