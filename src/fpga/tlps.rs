use c2rust_bitfields::*;
use dataview::{DataView, Pod};

const TLP_READ_32: u8 = 0x00;
const TLP_READ_64: u8 = 0x20;

const fn pack_bits4(first: u8, second: u8) -> u8 {
    (first & ((1 << 4) - 1)) + ((second & ((1 << 4) - 1)) << 4)
}

#[repr(C, align(1))]
#[derive(BitfieldStruct, Pod)]
pub struct TlpHeader {
    #[bitfield(name = "len_tlps", ty = "libc::uint16_t", bits = "0..=9")]
    #[bitfield(name = "at", ty = "libc::uint16_t", bits = "10..=11")]
    #[bitfield(name = "attr", ty = "libc::uint16_t", bits = "12..=13")]
    #[bitfield(name = "ep", ty = "libc::uint16_t", bits = "14..=14")]
    #[bitfield(name = "td", ty = "libc::uint16_t", bits = "15..=15")]
    #[bitfield(name = "r1", ty = "libc::uint8_t", bits = "16..=19")]
    #[bitfield(name = "tc", ty = "libc::uint8_t", bits = "20..=22")]
    #[bitfield(name = "r2", ty = "libc::uint8_t", bits = "23..=23")]
    #[bitfield(name = "ty", ty = "libc::uint8_t", bits = "24..=31")]
    buffer: [u8; 4],
}
const _: [(); core::mem::size_of::<TlpHeader>()] = [(); 4];

impl TlpHeader {
    pub fn new(ty: u8, len_tlps: u16) -> Self {
        let mut h = Self::zeroed();
        h.set_ty(ty);
        if len_tlps < 0x1000 {
            // shift length
            h.set_len_tlps(len_tlps / 4);
        } else {
            // max length
            h.set_len_tlps(len_tlps);
        }
        h
    }
}

#[repr(C)]
#[derive(Pod)]
pub struct TlpReadWrite32 {
    header: TlpHeader,
    be: u8,
    tag: u8,
    requester_id: u16,
    address: u32,
}
const _: [(); core::mem::size_of::<TlpReadWrite32>()] = [(); 0xC];

#[allow(unused)]
impl TlpReadWrite32 {
    pub fn new_read(address: u32, len: u16, tag: u8, requester_id: u16) -> Self {
        Self {
            header: TlpHeader::new(TLP_READ_32, len),
            be: pack_bits4(0xF, 0xF),
            tag,
            requester_id,
            address,
        }
    }

    pub fn set_be(&mut self, first: u8, second: u8) {
        self.be = pack_bits4(first, second);
    }

    pub fn set_tag(&mut self, tag: u8) {
        self.tag = tag;
    }

    pub fn set_requester_id(&mut self, id: u16) {
        self.requester_id = id;
    }

    pub fn set_address(&mut self, address: u32) {
        self.address = address;
    }
}

#[repr(C)]
#[derive(Pod)]
pub struct TlpReadWrite64 {
    header: TlpHeader,
    be: u8,
    tag: u8,
    requester_id: u16,
    address_high: u32,
    address_low: u32,
}
const _: [(); core::mem::size_of::<TlpReadWrite64>()] = [(); 0x10];

#[allow(unused)]
impl TlpReadWrite64 {
    pub fn new_read(address: u64, len: u16, tag: u8, requester_id: u16) -> Self {
        Self {
            header: TlpHeader::new(TLP_READ_64, len),
            be: pack_bits4(0xF, 0xF),
            tag,
            requester_id,
            address_high: (address >> 32) as u32,
            address_low: address as u32,
        }
    }

    pub fn set_be(&mut self, first: u8, second: u8) {
        self.be = pack_bits4(first, second);
    }

    pub fn set_tag(&mut self, tag: u8) {
        self.tag = tag;
    }

    pub fn set_requester_id(&mut self, id: u16) {
        self.requester_id = id;
    }

    pub fn set_address(&mut self, address: u64) {
        self.address_high = (address >> 32) as u32;
        self.address_low = address as u32;
    }
}

/// Tlp Packet - Completion with Data
#[repr(C, align(1))]
#[derive(BitfieldStruct, Pod)]
pub struct TlpCplD {
    header: TlpHeader,
    #[bitfield(name = "byte_count", ty = "libc::uint16_t", bits = "0..=11")]
    #[bitfield(name = "attr", ty = "libc::uint16_t", bits = "12..=12")]
    #[bitfield(name = "ep", ty = "libc::uint16_t", bits = "13..=15")]
    buffer1: [u8; 2],
    completer_id: u16,
    #[bitfield(name = "lower_addr", ty = "libc::uint8_t", bits = "0..=6")]
    #[bitfield(name = "r1", ty = "libc::uint8_t", bits = "7..=7")]
    buffer2: [u8; 1],
    tag: u8,
    requester_id: u16,
}
//const _: [(); core::mem::size_of::<TlpCplD>()] = [(); 0x10];

#[cfg(test)]
mod tests {
    use super::{pack_bits4, TlpHeader, TlpReadWrite32, TlpReadWrite64, TLP_READ_32, TLP_READ_64};
    use dataview::Pod;
    use memflow::size;

    #[test]
    fn test_pack_bits4() {
        assert_eq!(pack_bits4(0xA, 0xB), 0xBA);
        assert_eq!(pack_bits4(0xAB, 0xCD), 0xDB);
    }

    #[test]
    fn test_header32() {
        let header = TlpHeader::new(TLP_READ_32, 0x123);
        assert_eq!(header.as_bytes(), &[72, 0, 0, 0])
    }

    #[test]
    fn test_header64() {
        let header = TlpHeader::new(TLP_READ_64, 0x123);
        assert_eq!(header.as_bytes(), &[72, 0, 0, 32])
    }

    #[test]
    fn test_tlp_rw32() {
        let tlp = TlpReadWrite32::new_read(0x6000, 0x123, 0x80, 17);
        assert_eq!(tlp.as_bytes(), &[72, 0, 0, 0, 255, 128, 17, 0, 0, 96, 0, 0])
    }

    #[test]
    fn test_tlp_rw64() {
        let tlp = TlpReadWrite64::new_read(size::gb(4) as u64 + 0x6000, 0x123, 0x80, 17);
        assert_eq!(
            tlp.as_bytes(),
            &[72, 0, 0, 32, 255, 128, 17, 0, 1, 0, 0, 0, 0, 96, 0, 0]
        )
    }
}
