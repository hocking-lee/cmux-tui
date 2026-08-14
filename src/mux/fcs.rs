//! 3GPP TS 27.010 使用的 CRC-8 FCS。
//!
//! 反射多项式 0xE0（即 x^8+x^2+x+1 的反射形式），初值 0xFF。
//! 发送时写入帧的字节是 `0xFF - crc(data)`；
//! 接收方对 `data ++ [fcs]` 再跑一遍 CRC，结果应恒等于 0xCF。

/// 接收端校验通过时 CRC 的固定结果值。
pub const FCS_GOOD: u8 = 0xCF;

const fn make_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u8;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xE0 } else { crc >> 1 };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static TABLE: [u8; 256] = make_table();

fn crc(data: &[u8]) -> u8 {
    let mut acc = 0xFFu8;
    for b in data {
        acc = TABLE[(acc ^ b) as usize];
    }
    acc
}

/// 计算应写入帧的 FCS 字节。
pub fn fcs(data: &[u8]) -> u8 {
    0xFF - crc(data)
}

/// 校验收到的 FCS 是否与数据匹配。
pub fn check(data: &[u8], received: u8) -> bool {
    let mut acc = crc(data);
    acc = TABLE[(acc ^ received) as usize];
    acc == FCS_GOOD
}

#[cfg(test)]
mod tests {
    use super::*;

    // 规范帧 F9 03 3F 01 1C F9：DLCI 0 上的 SABM
    #[test]
    fn sabm_dlci0_fcs_matches_spec() {
        assert_eq!(fcs(&[0x03, 0x3F, 0x01]), 0x1C);
    }

    // 规范帧 F9 03 73 01 D7 F9：DLCI 0 上的 UA
    #[test]
    fn ua_dlci0_fcs_matches_spec() {
        assert_eq!(fcs(&[0x03, 0x73, 0x01]), 0xD7);
    }

    #[test]
    fn check_accepts_correct_fcs() {
        assert!(check(&[0x03, 0x3F, 0x01], 0x1C));
        assert!(check(&[0x03, 0x73, 0x01], 0xD7));
    }

    #[test]
    fn check_rejects_corrupted_fcs() {
        assert!(!check(&[0x03, 0x3F, 0x01], 0x1D));
        assert!(!check(&[0x03, 0x3F, 0x02], 0x1C));
    }

    #[test]
    fn generated_fcs_always_passes_check() {
        for len in 0..16usize {
            let data: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(37)).collect();
            let f = fcs(&data);
            assert!(check(&data, f), "len={len} 自洽性失败");
        }
    }
}
