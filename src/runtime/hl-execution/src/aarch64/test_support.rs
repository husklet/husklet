use crate::PcCoordinatePort;

pub(crate) struct Coordinates {
    pub(crate) low: u64,
    pub(crate) high: u64,
    pub(crate) size: u64,
}

impl PcCoordinatePort for Coordinates {
    fn architectural_pc(&self, execution_pc: u64) -> u64 {
        if execution_pc >= self.high && execution_pc < self.high + self.size {
            execution_pc - self.high + self.low
        } else {
            execution_pc
        }
    }
}
