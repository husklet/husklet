mod abi;

pub use abi::{Abi as MqAbi, Attributes as MqAttributes, Error as MqError, Event as MqEvent, Notify as MqNotify};
pub use abi::{
    ReceiveDestination as MqReceiveDestination, StagedAttributes as MqStagedAttributes, Timespec as MqTimespec,
};

#[cfg(test)]
mod test;
