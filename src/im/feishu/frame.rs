//! Hand-written protobuf codec for Feishu long-connection frames.
//!
//! Field numbers match the official SDK `pbbp2.Frame`. The ping golden bytes
//! are the official Python SDK serialization of a service-7 ping.

pub(crate) const CONTROL: i32 = 0;
pub(crate) const DATA: i32 = 1;
pub(crate) const HEADER_TYPE: &str = "type";
pub(crate) const HEADER_MESSAGE_ID: &str = "message_id";
pub(crate) const HEADER_SUM: &str = "sum";
pub(crate) const HEADER_SEQ: &str = "seq";
pub(crate) const HEADER_BIZ_RT: &str = "biz_rt";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Frame {
    pub(crate) seq_id: u64,
    pub(crate) log_id: u64,
    pub(crate) service: i32,
    pub(crate) method: i32,
    pub(crate) headers: Vec<Header>,
    pub(crate) payload_encoding: Option<String>,
    pub(crate) payload_type: Option<String>,
    pub(crate) payload: Option<Vec<u8>>,
    pub(crate) log_id_new: Option<String>,
}

impl Frame {
    pub(crate) fn ping(service: i32) -> Self {
        Self {
            seq_id: 0,
            log_id: 0,
            service,
            method: CONTROL,
            headers: vec![Header {
                key: HEADER_TYPE.into(),
                value: "ping".into(),
            }],
            payload_encoding: None,
            payload_type: None,
            payload: None,
            log_id_new: None,
        }
    }

    pub(crate) fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.key == key)
            .map(|header| header.value.as_str())
    }

    pub(crate) fn acknowledge(mut self, elapsed_ms: u128, code: u16) -> Self {
        self.headers.push(Header {
            key: HEADER_BIZ_RT.into(),
            value: elapsed_ms.to_string(),
        });
        self.payload = Some(format!(r#"{{"code":{code}}}"#).into_bytes());
        self
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_varint(1, 0, self.seq_id, &mut out);
        write_varint(2, 0, self.log_id, &mut out);
        write_varint(3, 0, self.service as u64, &mut out);
        write_varint(4, 0, self.method as u64, &mut out);
        for header in &self.headers {
            let mut message = Vec::new();
            write_bytes(1, header.key.as_bytes(), &mut message);
            write_bytes(2, header.value.as_bytes(), &mut message);
            write_bytes(5, &message, &mut out);
        }
        if let Some(value) = &self.payload_encoding {
            write_bytes(6, value.as_bytes(), &mut out);
        }
        if let Some(value) = &self.payload_type {
            write_bytes(7, value.as_bytes(), &mut out);
        }
        if let Some(value) = &self.payload {
            write_bytes(8, value, &mut out);
        }
        if let Some(value) = &self.log_id_new {
            write_bytes(9, value.as_bytes(), &mut out);
        }
        out
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ()> {
        let mut cursor = 0;
        let mut frame = Self {
            seq_id: 0,
            log_id: 0,
            service: 0,
            method: 0,
            headers: Vec::new(),
            payload_encoding: None,
            payload_type: None,
            payload: None,
            log_id_new: None,
        };
        while cursor < bytes.len() {
            let key = read_varint(bytes, &mut cursor)?;
            let field = (key >> 3) as u32;
            let wire = (key & 0x07) as u32;
            match (field, wire) {
                (1, 0) => frame.seq_id = read_varint(bytes, &mut cursor)?,
                (2, 0) => frame.log_id = read_varint(bytes, &mut cursor)?,
                (3, 0) => frame.service = read_varint(bytes, &mut cursor)? as i32,
                (4, 0) => frame.method = read_varint(bytes, &mut cursor)? as i32,
                (5, 2) => frame
                    .headers
                    .push(decode_header(&read_bytes(bytes, &mut cursor)?)?),
                (6, 2) => {
                    frame.payload_encoding = Some(read_string(bytes, &mut cursor)?);
                }
                (7, 2) => frame.payload_type = Some(read_string(bytes, &mut cursor)?),
                (8, 2) => frame.payload = Some(read_bytes(bytes, &mut cursor)?),
                (9, 2) => frame.log_id_new = Some(read_string(bytes, &mut cursor)?),
                _ => skip(bytes, &mut cursor, wire)?,
            }
        }
        Ok(frame)
    }
}

fn decode_header(bytes: &[u8]) -> Result<Header, ()> {
    let mut cursor = 0;
    let mut key = None;
    let mut value = None;
    while cursor < bytes.len() {
        let tag = read_varint(bytes, &mut cursor)?;
        let field = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u32;
        match (field, wire) {
            (1, 2) => key = Some(read_string(bytes, &mut cursor)?),
            (2, 2) => value = Some(read_string(bytes, &mut cursor)?),
            _ => skip(bytes, &mut cursor, wire)?,
        }
    }
    Ok(Header {
        key: key.ok_or(())?,
        value: value.ok_or(())?,
    })
}

fn write_varint(field: u32, wire: u32, value: u64, out: &mut Vec<u8>) {
    write_raw_varint(u64::from((field << 3) | wire), out);
    write_raw_varint(value, out);
}

fn write_bytes(field: u32, bytes: &[u8], out: &mut Vec<u8>) {
    write_raw_varint(u64::from((field << 3) | 2), out);
    write_raw_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn write_raw_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, ()> {
    let mut value = 0u64;
    let mut shift = 0;
    for _ in 0..10 {
        let byte = *bytes.get(*cursor).ok_or(())?;
        *cursor += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
    Err(())
}

fn read_bytes(bytes: &[u8], cursor: &mut usize) -> Result<Vec<u8>, ()> {
    let length = usize::try_from(read_varint(bytes, cursor)?).map_err(|_| ())?;
    let end = cursor.checked_add(length).ok_or(())?;
    if end > bytes.len() {
        return Err(());
    }
    let slice = bytes[*cursor..end].to_vec();
    *cursor = end;
    Ok(slice)
}

fn read_string(bytes: &[u8], cursor: &mut usize) -> Result<String, ()> {
    String::from_utf8(read_bytes(bytes, cursor)?).map_err(|_| ())
}

fn skip(bytes: &[u8], cursor: &mut usize, wire: u32) -> Result<(), ()> {
    match wire {
        0 => {
            read_varint(bytes, cursor)?;
        }
        1 => *cursor = cursor.checked_add(8).ok_or(())?,
        2 => {
            read_bytes(bytes, cursor)?;
        }
        5 => *cursor = cursor.checked_add(4).ok_or(())?,
        _ => return Err(()),
    }
    if *cursor > bytes.len() {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_matches_the_official_sdk_bytes() {
        let expected = [
            0x08, 0x00, 0x10, 0x00, 0x18, 0x07, 0x20, 0x00, 0x2a, 0x0c, 0x0a, 0x04, b't', b'y',
            b'p', b'e', 0x12, 0x04, b'p', b'i', b'n', b'g',
        ];
        assert_eq!(Frame::ping(7).encode(), expected);
        assert_eq!(Frame::decode(&expected).unwrap(), Frame::ping(7));
    }

    #[test]
    fn event_frame_round_trips_and_acknowledges() {
        let frame = Frame {
            seq_id: 4,
            log_id: 9,
            service: 7,
            method: DATA,
            headers: vec![Header {
                key: HEADER_TYPE.into(),
                value: "event".into(),
            }],
            payload_encoding: None,
            payload_type: None,
            payload: Some(b"{}".to_vec()),
            log_id_new: None,
        };
        let decoded = Frame::decode(&frame.encode()).unwrap();
        assert_eq!(decoded, frame);
        let ack = decoded.acknowledge(12, 200);
        assert_eq!(ack.header(HEADER_BIZ_RT), Some("12"));
        assert_eq!(ack.payload.as_deref(), Some(br#"{"code":200}"#.as_slice()));
        assert_eq!(Frame::decode(&ack.encode()).unwrap(), ack);
    }
}
