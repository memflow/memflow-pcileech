use bitfield::bitfield;
use dataview::{DataView, Pod};

const TLP_READ_32: u8 = 0x00;
const TLP_READ_64: u8 = 0x20;

const fn pack_bits4(first: u8, second: u8) -> u8 {
    (first & ((1 << 4) - 1)) + ((second & ((1 << 4) - 1)) << 4)
}

const fn split_addr64_high(address: u64) -> u32 {
    ((address & ((1 << 32) - 1)) >> 32) as u32
}

const fn split_addr64_low(address: u64) -> u32 {
    (address & ((1 << 32) - 1)) as u32
}

bitfield! {
    pub struct TlpHeader(u32);
    impl Debug;
    pub len_tlps, set_len_tlps: 9, 0;
    pub at, _: 11, 10;
    pub attr, _: 13, 12;
    pub ep, _: 14;
    pub td, _: 15;
    pub r1, _: 19, 16;
    pub tc, _: 22, 20;
    pub r2, _: 23;
    pub ty, set_ty: 31, 24;
}
const _: [(); core::mem::size_of::<TlpHeader>()] = [(); 4];

unsafe impl Pod for TlpHeader {}

impl TlpHeader {
    pub fn new(ty: u8, len_tlps: u16) -> Self {
        let mut h = Self { 0: 0 };

        println!("ok0");
        h.set_ty(ty as u32);
        println!("ok1");
        h.set_len_tlps(len_tlps as u32);
        println!("ok2");
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
            header: TlpHeader::new(TLP_READ_32, len / 4),
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
            header: TlpHeader::new(TLP_READ_64, len / 4),
            be: pack_bits4(0xF, 0xF),
            tag,
            requester_id,
            address_high: split_addr64_high(address),
            address_low: split_addr64_low(address),
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
        self.address_high = split_addr64_high(address);
        self.address_low = split_addr64_low(address);
    }
}

#[cfg(test)]
mod tests {
    use super::pack_bits4;

    #[test]
    fn test_pack_bits4() {
        assert_eq!(pack_bits4(0xA, 0xB), 0xBA);
        assert_eq!(pack_bits4(0xAB, 0xCD), 0xDB);
    }
}
