#![allow(unused)] // TODO remove

use std::ops::Range;

pub mod instruction;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Register {
    Cpu(CpuRegister),
    Special(SpecialRegister),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum CpuRegister {
    A0 = 0,
    A1 = 1,
    A2 = 2,
    A3 = 3,
    A4 = 4,
    A5 = 5,
    A6 = 6,
    A7 = 7,
    A8 = 8,
    A9 = 9,
    A10 = 10,
    A11 = 11,
    A12 = 12,
    A13 = 13,
    A14 = 14,
    A15 = 15,
}

impl CpuRegister {
    pub const fn scratch() -> Self {
        Self::A3
    }

    pub const fn address(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum SpecialRegister {
    Lbeg = 0,
    Lend = 1,
    Lcount = 2,
    Sar = 3,
    Br = 4,
    Litbase = 5,
    Scompare1 = 12,
    AccLo = 16,
    AccHi = 17,
    M0 = 32,
    M1 = 33,
    M2 = 34,
    M3 = 35,
    Windowbase = 72,
    Windowstart = 73,
    PteVAddr = 83,
    RAsid = 90,
    // MpuEnB = 90,
    ITlbCfg = 91,
    DTlbCfg = 92,
    // MpuCfg = 92,
    ERAccess = 95,
    IBreakEnable = 96,
    Memctl = 97,
    CacheAdrDis = 98,
    AtomCtl = 99,
    Ddr = 104,
    Mepc = 106,
    Meps = 107,
    Mesave = 108,
    Mesr = 109,
    Mecr = 110,
    MeVAddr = 111,
    IBreakA0 = 128,
    IBreakA1 = 129,
    DBreakA0 = 144,
    DBreakA1 = 145,
    DBreakC0 = 160,
    DBreakC1 = 161,
    Epc1 = 177,
    Epc2 = 178,
    Epc3 = 179,
    Epc4 = 180,
    Epc5 = 181,
    Epc6 = 182,
    Epc7 = 183,
    IBreakC0 = 192,
    IBreakC1 = 193,
    // Depc = 192,
    Eps2 = 194,
    Eps3 = 195,
    Eps4 = 196,
    Eps5 = 197,
    Eps6 = 198,
    Eps7 = 199,
    ExcSave1 = 209,
    ExcSave2 = 210,
    ExcSave3 = 211,
    ExcSave4 = 212,
    ExcSave5 = 213,
    ExcSave6 = 214,
    ExcSave7 = 215,
    CpEnable = 224,
    // Interrupt = 226,
    IntSet = 226,
    IntClear = 227,
    IntEnable = 228,
    Ps = 230,
    VecBase = 231,
    ExcCause = 232,
    DebugCause = 233,
    CCount = 234,
    Prid = 235,
    ICount = 236,
    ICountLevel = 237,
    ExcVaddr = 238,
    CCompare0 = 240,
    CCompare1 = 241,
    CCompare2 = 242,
    Misc0 = 244,
    Misc1 = 245,
    Misc2 = 246,
    Misc3 = 247,
}

#[allow(non_upper_case_globals)] // Aliasses have same style as other register names
impl SpecialRegister {
    // Aliasses
    pub const MpuEnB: Self = Self::RAsid;
    pub const MpuCfg: Self = Self::DTlbCfg;
    pub const Depc: Self = Self::IBreakC0;
    pub const Interrupt: Self = Self::IntSet;

    pub const fn address(self) -> u8 {
        self as u8
    }
}

pub struct MemoryRegion {
    pub addr: u32,
    pub size: u32,
}

impl MemoryRegion {
    fn as_range(&self) -> Range<u32> {
        self.addr..self.addr + self.size
    }

    fn contains(&self, addr: u32) -> bool {
        self.as_range().contains(&addr)
    }
}

pub struct MemoryConfig {
    pub regions: Vec<MemoryRegion>,
}

impl MemoryConfig {
    fn is_cacheable(&self, address: u32) -> bool {
        self.regions.iter().any(|region| region.contains(address))
    }
}

pub struct CacheConfig {
    pub line_size: u32,
    pub size: u32,
    pub way_count: u8,
}

pub struct ChipConfig {
    pub icache: CacheConfig,
    pub dcache: CacheConfig,

    pub sram: MemoryConfig,
    pub srom: MemoryConfig,
    pub iram: MemoryConfig,
    pub irom: MemoryConfig,
    pub dram: MemoryConfig,
    pub drom: MemoryConfig,
}

impl ChipConfig {
    pub fn is_icacheable(&self, address: u32) -> bool {
        if self.icache.size == 0 {
            return false;
        }
        self.iram.is_cacheable(address)
            || self.irom.is_cacheable(address)
            || self.sram.is_cacheable(address)
            || self.srom.is_cacheable(address)
    }

    pub fn is_dcacheable(&self, address: u32) -> bool {
        if self.dcache.size == 0 {
            return false;
        }
        self.dram.is_cacheable(address)
            || self.drom.is_cacheable(address)
            || self.sram.is_cacheable(address)
            || self.srom.is_cacheable(address)
    }
}
