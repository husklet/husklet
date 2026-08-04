use hl_linux::{Errno, EventMarshalError};

pub(crate) struct ErrorMap;

impl ErrorMap {
    pub(crate) fn marshal(error: EventMarshalError) -> Errno {
        match error {
            EventMarshalError::Marshal(error) => error.errno(),
            EventMarshalError::Invalid => Errno::EINVAL,
            EventMarshalError::Overflow => Errno::EOVERFLOW,
        }
    }
}
