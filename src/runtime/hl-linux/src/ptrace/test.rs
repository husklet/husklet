use super::{Options, Plan, Request, Resume};

#[test]
fn baseline_requests() {
    assert_eq!(Request::decode([0, 0, 0, 0, 0, 0]), Request::Supported(Plan::TraceMe));
    assert_eq!(
        Request::decode([0x4206, 71, 0, 0x11, 0, 0]),
        Request::Supported(Plan::Seize {
            process: 71,
            options: Options::from_bits(0x11)
        })
    );
    assert_eq!(
        Request::decode([24, 72, 0, 5, 0, 0]),
        Request::Supported(Plan::Resume {
            process: 72,
            signal: 5,
            mode: Resume::Syscall
        })
    );
}

#[test]
fn memory_requests() {
    assert_eq!(
        Request::decode([2, 9, 0x1000, 0x2000, 0, 0]),
        Request::Supported(Plan::PeekData {
            process: 9,
            address: 0x1000,
            destination: 0x2000
        })
    );
    assert_eq!(
        Request::decode([0x4204, 9, 1, 0x3000, 0, 0]),
        Request::Supported(Plan::GetRegisterSet {
            process: 9,
            note: 1,
            iovec: 0x3000
        })
    );
}

#[test]
fn unknown_request() {
    assert_eq!(Request::decode([10, 0, 0, 0, 0, 0]), Request::Unsupported);
}
