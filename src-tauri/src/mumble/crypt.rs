//! Mumble's UDP voice encryption: OCB2 with AES-128, a port of Mumble's CryptStateOCB2.
//! Each packet = 1 byte IV low byte + 3 bytes tag + ciphertext. Includes Mumble's
//! counter-measure for the XEX* attack (eprint 2019/311, section 9).
use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;

const B: usize = 16;

#[derive(Clone)]
pub struct CryptState {
    cipher: Option<Aes128>,
    pub encrypt_iv: [u8; B],
    pub decrypt_iv: [u8; B],
    history: [u8; 256],
    pub good: u32,
    pub late: u32,
    pub lost: u32,
    pub resync: u32,
}

fn s2(b: &mut [u8; B]) {
    let v = u128::from_be_bytes(*b);
    let carry = v >> 127;
    *b = ((v << 1) ^ (carry * 0x87)).to_be_bytes();
}

fn s3(b: &mut [u8; B]) {
    let v = u128::from_be_bytes(*b);
    let carry = v >> 127;
    *b = (v ^ ((v << 1) ^ (carry * 0x87))).to_be_bytes();
}

fn xor(dst: &mut [u8; B], a: &[u8; B], b: &[u8; B]) {
    for i in 0..B {
        dst[i] = a[i] ^ b[i];
    }
}

fn block(s: &[u8]) -> [u8; B] {
    let mut b = [0u8; B];
    b.copy_from_slice(&s[..B]);
    b
}

impl Default for CryptState {
    fn default() -> Self {
        CryptState { cipher: None, encrypt_iv: [0; B], decrypt_iv: [0; B], history: [0; 256], good: 0, late: 0, lost: 0, resync: 0 }
    }
}

impl CryptState {
    pub fn is_valid(&self) -> bool {
        self.cipher.is_some()
    }

    /// From the server's CryptSetup: our encrypt IV is the client nonce, decrypt IV the server's.
    pub fn set_key(&mut self, key: &[u8], client_nonce: &[u8], server_nonce: &[u8]) -> bool {
        if key.len() != B || client_nonce.len() != B || server_nonce.len() != B {
            return false;
        }
        self.cipher = Some(Aes128::new(GenericArray::from_slice(key)));
        self.encrypt_iv.copy_from_slice(client_nonce);
        self.decrypt_iv.copy_from_slice(server_nonce);
        self.history = [0; 256];
        true
    }

    pub fn set_decrypt_iv(&mut self, iv: &[u8]) {
        if iv.len() == B {
            self.decrypt_iv.copy_from_slice(iv);
            self.resync += 1;
        }
    }

    fn aes_enc(&self, b: &mut [u8; B]) {
        if let Some(c) = &self.cipher {
            c.encrypt_block(GenericArray::from_mut_slice(b));
        }
    }

    fn aes_dec(&self, b: &mut [u8; B]) {
        if let Some(c) = &self.cipher {
            c.decrypt_block(GenericArray::from_mut_slice(b));
        }
    }

    fn ocb_encrypt(&self, plain: &[u8], out: &mut [u8], nonce: &[u8; B], tag: &mut [u8; B]) {
        let mut delta = *nonce;
        self.aes_enc(&mut delta);
        let mut checksum = [0u8; B];
        let mut tmp = [0u8; B];
        let mut len = plain.len();
        let mut off = 0;
        while len > B {
            // XEX* counter-measure: the second-to-last block must not be all zero but the last byte.
            let mut flip = false;
            if len - B <= B && plain[off..off + B - 1].iter().all(|&x| x == 0) {
                flip = true;
            }
            s2(&mut delta);
            let p = block(&plain[off..]);
            xor(&mut tmp, &delta, &p);
            if flip {
                tmp[0] ^= 1;
            }
            self.aes_enc(&mut tmp);
            let mut c = [0u8; B];
            xor(&mut c, &delta, &tmp);
            out[off..off + B].copy_from_slice(&c);
            let ck = checksum;
            xor(&mut checksum, &ck, &p);
            if flip {
                checksum[0] ^= 1;
            }
            len -= B;
            off += B;
        }
        s2(&mut delta);
        tmp = [0u8; B];
        tmp[8..].copy_from_slice(&((len as u64) * 8).to_be_bytes());
        let t = tmp;
        xor(&mut tmp, &t, &delta);
        let mut pad = tmp;
        self.aes_enc(&mut pad);
        tmp[..len].copy_from_slice(&plain[off..off + len]);
        tmp[len..].copy_from_slice(&pad[len..]);
        let ck = checksum;
        xor(&mut checksum, &ck, &tmp);
        let t = tmp;
        xor(&mut tmp, &pad, &t);
        out[off..off + len].copy_from_slice(&tmp[..len]);
        s3(&mut delta);
        xor(&mut tmp, &delta, &checksum);
        *tag = tmp;
        self.aes_enc(tag);
    }

    fn ocb_decrypt(&self, enc: &[u8], out: &mut [u8], nonce: &[u8; B], tag: &mut [u8; B]) -> bool {
        let mut delta = *nonce;
        self.aes_enc(&mut delta);
        let mut checksum = [0u8; B];
        let mut tmp = [0u8; B];
        let mut len = enc.len();
        let mut off = 0;
        let mut ok = true;
        while len > B {
            s2(&mut delta);
            let c = block(&enc[off..]);
            xor(&mut tmp, &delta, &c);
            self.aes_dec(&mut tmp);
            let mut p = [0u8; B];
            xor(&mut p, &delta, &tmp);
            out[off..off + B].copy_from_slice(&p);
            let ck = checksum;
            xor(&mut checksum, &ck, &p);
            len -= B;
            off += B;
        }
        s2(&mut delta);
        tmp = [0u8; B];
        tmp[8..].copy_from_slice(&((len as u64) * 8).to_be_bytes());
        let t = tmp;
        xor(&mut tmp, &t, &delta);
        let mut pad = tmp;
        self.aes_enc(&mut pad);
        tmp = [0u8; B];
        tmp[..len].copy_from_slice(&enc[off..off + len]);
        let t = tmp;
        xor(&mut tmp, &t, &pad);
        let ck = checksum;
        xor(&mut checksum, &ck, &tmp);
        out[off..off + len].copy_from_slice(&tmp[..len]);
        // XEX* counter-measure: a forged last block would equal delta (but its last byte).
        if tmp[..B - 1] == delta[..B - 1] {
            ok = false;
        }
        s3(&mut delta);
        xor(&mut tmp, &delta, &checksum);
        *tag = tmp;
        self.aes_enc(tag);
        ok
    }

    pub fn encrypt(&mut self, plain: &[u8]) -> Vec<u8> {
        for i in 0..B {
            self.encrypt_iv[i] = self.encrypt_iv[i].wrapping_add(1);
            if self.encrypt_iv[i] != 0 {
                break;
            }
        }
        let mut out = vec![0u8; plain.len() + 4];
        let mut tag = [0u8; B];
        let iv = self.encrypt_iv;
        self.ocb_encrypt(plain, &mut out[4..], &iv, &mut tag);
        out[0] = self.encrypt_iv[0];
        out[1..4].copy_from_slice(&tag[..3]);
        out
    }

    pub fn decrypt(&mut self, src: &[u8]) -> Option<Vec<u8>> {
        if src.len() < 4 || self.cipher.is_none() {
            return None;
        }
        let save = self.decrypt_iv;
        let ivbyte = src[0];
        let mut restore = false;
        let mut lost: i32 = 0;
        let mut late: u32 = 0;
        let d0 = self.decrypt_iv[0];
        if d0.wrapping_add(1) == ivbyte {
            // In order, as expected.
            if ivbyte > d0 {
                self.decrypt_iv[0] = ivbyte;
            } else if ivbyte < d0 {
                self.decrypt_iv[0] = ivbyte;
                for i in 1..B {
                    self.decrypt_iv[i] = self.decrypt_iv[i].wrapping_add(1);
                    if self.decrypt_iv[i] != 0 {
                        break;
                    }
                }
            } else {
                return None;
            }
        } else {
            // Out of order or a repeat.
            let mut diff = ivbyte as i32 - d0 as i32;
            if diff > 128 {
                diff -= 256;
            } else if diff < -128 {
                diff += 256;
            }
            if ivbyte < d0 && diff > -30 && diff < 0 {
                late = 1;
                lost = -1;
                self.decrypt_iv[0] = ivbyte;
                restore = true;
            } else if ivbyte > d0 && diff > -30 && diff < 0 {
                late = 1;
                lost = -1;
                self.decrypt_iv[0] = ivbyte;
                for i in 1..B {
                    let was = self.decrypt_iv[i];
                    self.decrypt_iv[i] = was.wrapping_sub(1);
                    if was != 0 {
                        break;
                    }
                }
                restore = true;
            } else if ivbyte > d0 && diff > 0 {
                lost = ivbyte as i32 - d0 as i32 - 1;
                self.decrypt_iv[0] = ivbyte;
            } else if ivbyte < d0 && diff > 0 {
                lost = 256 - d0 as i32 + ivbyte as i32 - 1;
                self.decrypt_iv[0] = ivbyte;
                for i in 1..B {
                    self.decrypt_iv[i] = self.decrypt_iv[i].wrapping_add(1);
                    if self.decrypt_iv[i] != 0 {
                        break;
                    }
                }
            } else {
                return None;
            }
            if self.history[self.decrypt_iv[0] as usize] == self.decrypt_iv[1] {
                self.decrypt_iv = save;
                return None;
            }
        }
        let mut out = vec![0u8; src.len() - 4];
        let mut tag = [0u8; B];
        let iv = self.decrypt_iv;
        let ok = self.ocb_decrypt(&src[4..], &mut out, &iv, &mut tag);
        if !ok || tag[..3] != src[1..4] {
            self.decrypt_iv = save;
            return None;
        }
        self.history[self.decrypt_iv[0] as usize] = self.decrypt_iv[1];
        if restore {
            self.decrypt_iv = save;
        }
        self.good += 1;
        self.late += late;
        if lost > 0 {
            self.lost += lost as u32;
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mumble's own test vectors (src/tests/TestCrypt, from draft-krovetz-ocb-00).
    #[test]
    fn mumble_vectors() {
        let key: [u8; 16] = core::array::from_fn(|i| i as u8);
        let mut cs = CryptState::default();
        cs.set_key(&key, &key, &key);
        let mut tag = [0u8; 16];
        cs.ocb_encrypt(&[], &mut [], &key, &mut tag);
        assert_eq!(tag, [0xBF, 0x31, 0x08, 0x13, 0x07, 0x73, 0xAD, 0x5E, 0xC7, 0x0E, 0xC6, 0x9E, 0x78, 0x75, 0xA7, 0xB0]);
        let src: Vec<u8> = (0..40u8).collect();
        let mut out = vec![0u8; 40];
        cs.ocb_encrypt(&src, &mut out, &key, &mut tag);
        assert_eq!(tag, [0x9D, 0xB0, 0xCD, 0xF8, 0x80, 0xF7, 0x3E, 0x3E, 0x10, 0xD4, 0xEB, 0x32, 0x17, 0x76, 0x66, 0x88]);
        assert_eq!(
            out,
            [
                0xF7, 0x5D, 0x6B, 0xC8, 0xB4, 0xDC, 0x8D, 0x66, 0xB8, 0x36, 0xA2, 0xB0, 0x8B, 0x32, 0xA6, 0x36, 0x9F, 0x1C, 0xD3, 0xC5, 0x22, 0x8D, 0x79, 0xFD,
                0x6C, 0x26, 0x7F, 0x5F, 0x6A, 0xA7, 0xB2, 0x31, 0xC7, 0xDF, 0xB9, 0xD5, 0x99, 0x51, 0xAE, 0x9C
            ]
        );
    }

    #[test]
    fn round_trip() {
        let key = [7u8; 16];
        let (cn, sn) = ([1u8; 16], [2u8; 16]);
        let mut a = CryptState::default();
        let mut b = CryptState::default();
        a.set_key(&key, &cn, &sn);
        b.set_key(&key, &sn, &cn); // the other side's view
        for len in [0usize, 1, 15, 16, 17, 31, 32, 33, 100, 1000] {
            let msg: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
            let enc = a.encrypt(&msg);
            assert_eq!(b.decrypt(&enc).as_deref(), Some(&msg[..]), "len {len}");
        }
        // a replayed packet is rejected
        let enc = a.encrypt(b"hello");
        assert!(b.decrypt(&enc).is_some());
        assert!(b.decrypt(&enc).is_none());
    }
}
