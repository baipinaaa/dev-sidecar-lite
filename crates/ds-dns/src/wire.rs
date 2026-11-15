//! DNS 报文构造与解析（只覆盖本项目需要的部分）。
//!
//! - 构造：A / AAAA / HTTPS(65) 查询
//! - 解析：任意 RR + SVCB/HTTPS 参数（含 ECH，key=5）

use std::net::{Ipv4Addr, Ipv6Addr};

use anyhow::{bail, Result};

pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_HTTPS: u16 = 65;

/// SVCB 中 ECH 参数的 key。
pub const SVCB_KEY_ECH: u16 = 5;
/// SVCB 中 alpn 参数的 key。
pub const SVCB_KEY_ALPN: u16 = 1;
/// SVCB 中 ipv4hint 参数的 key。
pub const SVCB_KEY_IPV4HINT: u16 = 4;

/// 构造查询报文。
pub fn build_query(id: u16, name: &str, qtype: u16) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(64);

    buf.extend_from_slice(&id.to_be_bytes());
    buf.extend_from_slice(&0x0100u16.to_be_bytes()); // RD=1
    buf.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    buf.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    buf.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    buf.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT

    encode_name(&mut buf, name)?;
    buf.extend_from_slice(&qtype.to_be_bytes());
    buf.extend_from_slice(&1u16.to_be_bytes()); // IN

    Ok(buf)
}

/// 把域名编码成 DNS 名字格式。
pub fn encode_name(buf: &mut Vec<u8>, name: &str) -> Result<()> {
    let name = name.trim().trim_end_matches('.');
    if name.is_empty() {
        buf.push(0);
        return Ok(());
    }

    for label in name.split('.') {
        if label.is_empty() {
            bail!("域名 {name} 含空标签");
        }
        if label.len() > 63 {
            bail!("域名 {name} 的单标签超过 63 字节");
        }
        buf.push(label.len() as u8);
        buf.extend_from_slice(label.as_bytes());
    }
    buf.push(0);
    Ok(())
}

/// 一条资源记录。
#[derive(Debug, Clone)]
pub struct Answer {
    pub name: String,
    pub rtype: u16,
    pub ttl: u32,
    pub rdata: Vec<u8>,
}

impl Answer {
    /// A 记录 → IPv4（非 A 记录返回 None）。
    pub fn as_ipv4(&self) -> Option<Ipv4Addr> {
        if self.rtype != TYPE_A || self.rdata.len() != 4 {
            return None;
        }
        Some(Ipv4Addr::new(
            self.rdata[0],
            self.rdata[1],
            self.rdata[2],
            self.rdata[3],
        ))
    }

    /// AAAA 记录 → IPv6。
    pub fn as_ipv6(&self) -> Option<Ipv6Addr> {
        if self.rtype != TYPE_AAAA || self.rdata.len() != 16 {
            return None;
        }
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&self.rdata);
        Some(Ipv6Addr::from(bytes))
    }

    /// HTTPS/SVCB 记录 → 解析后的 SVCB。
    pub fn as_svcb(&self) -> Option<Svcb> {
        if self.rtype != TYPE_HTTPS {
            return None;
        }
        parse_svcb(&self.rdata).ok()
    }
}

/// 解析响应报文，返回 answer + additional 里的全部记录。
pub fn parse_response(buf: &[u8]) -> Result<Vec<Answer>> {
    if buf.len() < 12 {
        bail!("DNS 响应短于 12 字节");
    }

    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    let rcode = flags & 0x000F;
    if rcode != 0 {
        bail!("DNS 响应状态码 rcode={rcode}");
    }

    let qdcount = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let ancount = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let arcount = u16::from_be_bytes([buf[10], buf[11]]) as usize;

    let mut pos = 12;

    for _ in 0..qdcount {
        let (_, next) = read_name(buf, pos)?;
        pos = next + 4; // QTYPE + QCLASS
        if pos > buf.len() {
            bail!("问题段越界");
        }
    }

    let mut answers = Vec::new();
    for _ in 0..(ancount + arcount) {
        if pos >= buf.len() {
            break;
        }
        let (name, next) = read_name(buf, pos)?;
        pos = next;

        if pos + 10 > buf.len() {
            bail!("资源记录头越界");
        }

        let rtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let ttl = u32::from_be_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]);
        let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;

        if pos + rdlen > buf.len() {
            bail!("资源记录数据越界（rdlen={rdlen}）");
        }

        answers.push(Answer {
            name,
            rtype,
            ttl,
            rdata: buf[pos..pos + rdlen].to_vec(),
        });

        pos += rdlen;
    }

    Ok(answers)
}

/// 解析后的 SVCB/HTTPS 记录。
#[derive(Debug, Clone)]
pub struct Svcb {
    pub priority: u16,
    pub target: String,
    pub params: Vec<(u16, Vec<u8>)>,
}

impl Svcb {
    /// ECHConfigList（key=5）。
    pub fn ech(&self) -> Option<&[u8]> {
        self.param(SVCB_KEY_ECH)
    }

    /// ALPN 列表。
    pub fn alpn(&self) -> Vec<String> {
        let Some(value) = self.param(SVCB_KEY_ALPN) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < value.len() {
            let len = value[pos] as usize;
            pos += 1;
            if pos + len > value.len() {
                break;
            }
            out.push(String::from_utf8_lossy(&value[pos..pos + len]).to_string());
            pos += len;
        }
        out
    }

    /// ipv4hint。
    pub fn ipv4_hints(&self) -> Vec<Ipv4Addr> {
        let Some(value) = self.param(SVCB_KEY_IPV4HINT) else {
            return Vec::new();
        };
        value
            .chunks_exact(4)
            .map(|chunk| Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]))
            .collect()
    }

    pub fn param(&self, key: u16) -> Option<&[u8]> {
        self.params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_slice())
    }
}

/// 解析 SVCB/HTTPS 记录的 rdata。
pub fn parse_svcb(rdata: &[u8]) -> Result<Svcb> {
    if rdata.len() < 3 {
        bail!("SVCB rdata 过短");
    }

    let priority = u16::from_be_bytes([rdata[0], rdata[1]]);
    let (target, mut pos) = read_name(rdata, 2)?;

    let mut params = Vec::new();
    while pos + 4 <= rdata.len() {
        let key = u16::from_be_bytes([rdata[pos], rdata[pos + 1]]);
        let len = u16::from_be_bytes([rdata[pos + 2], rdata[pos + 3]]) as usize;
        pos += 4;

        if pos + len > rdata.len() {
            bail!("SVCB 参数越界（key={key} len={len}）");
        }
        params.push((key, rdata[pos..pos + len].to_vec()));
        pos += len;
    }

    Ok(Svcb {
        priority,
        target,
        params,
    })
}

/// 读一个（可能被压缩的）域名，返回 (名字, 下一个位置)。
fn read_name(buf: &[u8], start: usize) -> Result<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut pos = start;
    let mut next_after = start;
    let mut jumped = false;
    let mut hops = 0;

    loop {
        if pos >= buf.len() {
            bail!("域名越界");
        }

        let len = buf[pos];

        if len & 0xC0 == 0xC0 {
            if pos + 1 >= buf.len() {
                bail!("压缩指针越界");
            }
            let pointer = (((len & 0x3F) as usize) << 8) | buf[pos + 1] as usize;
            if !jumped {
                next_after = pos + 2;
                jumped = true;
            }
            hops += 1;
            if hops > 16 {
                bail!("域名压缩指针循环");
            }
            pos = pointer;
            continue;
        }

        if len == 0 {
            if !jumped {
                next_after = pos + 1;
            }
            break;
        }

        let end = pos + 1 + len as usize;
        if end > buf.len() {
            bail!("标签越界");
        }
        labels.push(String::from_utf8_lossy(&buf[pos + 1..end]).to_string());
        pos = end;
    }

    Ok((labels.join("."), next_after))
}

/// 生成一个简单的查询 id（避免全部为 0）。
pub fn next_query_id() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static COUNTER: AtomicU16 = AtomicU16::new(0x1A2B);
    COUNTER.fetch_add(1, Ordering::Relaxed) | 0x0100
}

/// 把响应里的 A/AAAA 记录抽成 IP 字符串。
pub fn collect_ips(answers: &[Answer]) -> Vec<String> {
    answers
        .iter()
        .filter_map(|answer| {
            if let Some(ip) = answer.as_ipv4() {
                Some(ip.to_string())
            } else {
                answer.as_ipv6().map(|ip| ip.to_string())
            }
        })
        .collect()
}

/// 从响应里取出第一个可用的 ECHConfigList。
pub fn find_ech(answers: &[Answer]) -> Option<Vec<u8>> {
    answers
        .iter()
        .filter_map(|answer| answer.as_svcb())
        .find_map(|svcb| svcb.ech().map(|bytes| bytes.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_layout_is_correct() {
        let q = build_query(0x1234, "example.com", TYPE_HTTPS).unwrap();
        assert_eq!(&q[0..2], &[0x12, 0x34]);
        assert_eq!(&q[2..4], &[0x01, 0x00]);
        assert_eq!(&q[4..6], &[0x00, 0x01]);
        assert_eq!(&q[12..13], &[7]);
        assert_eq!(&q[13..20], b"example");
        assert_eq!(&q[20..21], &[3]);
        assert_eq!(&q[21..24], b"com");
        assert_eq!(q[24], 0);
        assert_eq!(&q[25..27], &TYPE_HTTPS.to_be_bytes());
    }

    #[test]
    fn encodes_root_name() {
        let mut buf = Vec::new();
        encode_name(&mut buf, ".").unwrap();
        assert_eq!(buf, vec![0]);
    }

    #[test]
    fn rejects_bad_labels() {
        let mut buf = Vec::new();
        assert!(encode_name(&mut buf, "a..b").is_err());
    }

    /// 手工构造一个含 A 记录的回答。
    fn sample_a_response() -> Vec<u8> {
        let mut r = vec![
            0x12, 0x34, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        ];
        // question: example.com A IN
        encode_name(&mut r, "example.com").unwrap();
        r.extend_from_slice(&TYPE_A.to_be_bytes());
        r.extend_from_slice(&1u16.to_be_bytes());
        // answer: 指针 -> 名字，A，IN，ttl 60，rdlen 4，1.2.3.4
        r.extend_from_slice(&[0xC0, 0x0C]);
        r.extend_from_slice(&TYPE_A.to_be_bytes());
        r.extend_from_slice(&1u16.to_be_bytes());
        r.extend_from_slice(&60u32.to_be_bytes());
        r.extend_from_slice(&4u16.to_be_bytes());
        r.extend_from_slice(&[1, 2, 3, 4]);
        r
    }

    #[test]
    fn parses_a_record_with_compression() {
        let answers = parse_response(&sample_a_response()).unwrap();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].name, "example.com");
        assert_eq!(answers[0].as_ipv4().unwrap().to_string(), "1.2.3.4");
        assert_eq!(answers[0].ttl, 60);
    }

    #[test]
    fn parses_svcb_with_ech() {
        let ech = vec![0x00, 0x0D, 0x00, 0x01, 0x00, 0x03, b'a', b'b', b'c'];
        let mut rdata = Vec::new();
        rdata.extend_from_slice(&1u16.to_be_bytes()); // priority
        encode_name(&mut rdata, "cloudflare-ech.com").unwrap();
        rdata.extend_from_slice(&SVCB_KEY_ECH.to_be_bytes());
        rdata.extend_from_slice(&(ech.len() as u16).to_be_bytes());
        rdata.extend_from_slice(&ech);

        let svcb = parse_svcb(&rdata).unwrap();
        assert_eq!(svcb.priority, 1);
        assert_eq!(svcb.target, "cloudflare-ech.com");
        assert_eq!(svcb.ech().unwrap(), ech.as_slice());
    }

    #[test]
    fn parses_alpn_and_ipv4hint() {
        let mut rdata = Vec::new();
        rdata.extend_from_slice(&1u16.to_be_bytes());
        encode_name(&mut rdata, ".").unwrap();

        // alpn = ["h2", "h3"]
        let mut alpn = Vec::new();
        alpn.push(2u8);
        alpn.extend_from_slice(b"h2");
        alpn.push(2u8);
        alpn.extend_from_slice(b"h3");
        rdata.extend_from_slice(&SVCB_KEY_ALPN.to_be_bytes());
        rdata.extend_from_slice(&(alpn.len() as u16).to_be_bytes());
        rdata.extend_from_slice(&alpn);

        rdata.extend_from_slice(&SVCB_KEY_IPV4HINT.to_be_bytes());
        rdata.extend_from_slice(&8u16.to_be_bytes());
        rdata.extend_from_slice(&[104, 16, 252, 55, 172, 66, 166, 61]);

        let svcb = parse_svcb(&rdata).unwrap();
        assert_eq!(svcb.target, "");
        assert_eq!(svcb.alpn(), vec!["h2".to_string(), "h3".to_string()]);
        let hints: Vec<String> = svcb.ipv4_hints().iter().map(|ip| ip.to_string()).collect();
        assert_eq!(hints, vec!["104.16.252.55", "172.66.166.61"]);
        assert!(svcb.ech().is_none());
    }

    #[test]
    fn rejects_truncated_rdata() {
        assert!(parse_svcb(&[0, 1]).is_err());
        assert!(parse_svcb(&[0, 1, 0, 0, 0, 0, 9, 9, 9]).is_err());
    }

    #[test]
    fn query_id_is_nonzero() {
        assert_ne!(next_query_id() & 0xFF, 0);
    }
}
