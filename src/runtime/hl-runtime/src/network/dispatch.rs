use hl_linux::{Errno, GuestMemory, LinuxResult, NetworkSyscalls, SyscallOperation};

use super::RuntimeNetworkSyscalls;
use crate::RuntimeNetworkHost;

impl<H: RuntimeNetworkHost, M: GuestMemory> NetworkSyscalls for RuntimeNetworkSyscalls<H, M> {
    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        match operation.name {
            "socket" => self.socket(arguments[0] as i32, arguments[1] as u32, arguments[2] as i32),
            "socketpair" => self.socketpair(
                arguments[0] as i32,
                arguments[1] as u32,
                arguments[2] as i32,
                arguments[3],
            ),
            "bind" => self.bind(arguments[0] as i32, arguments[1], arguments[2] as u32),
            "listen" => self.listen(arguments[0] as i32, arguments[1] as i32),
            "connect" => self.connect(arguments[0] as i32, arguments[1], arguments[2] as u32),
            "accept" => self.accept4(arguments[0] as i32, arguments[1], arguments[2], 0),
            "accept4" => self.accept4(arguments[0] as i32, arguments[1], arguments[2], arguments[3] as u32),
            "getsockname" => self.address(arguments[0] as i32, arguments[1], arguments[2], false),
            "getpeername" => self.address(arguments[0] as i32, arguments[1], arguments[2], true),
            "shutdown" => self.shutdown(arguments[0] as i32, arguments[1] as i32),
            "send" => self.send(arguments[0] as i32, arguments[1], arguments[2], arguments[3] as u32),
            "sendto" => self.sendto(
                arguments[0] as i32,
                arguments[1],
                arguments[2],
                arguments[3] as u32,
                arguments[4],
                arguments[5] as u32,
            ),
            "recvfrom" => self.recvfrom(
                arguments[0] as i32,
                arguments[1],
                arguments[2],
                arguments[3] as u32,
                arguments[4],
                arguments[5],
            ),
            "recv" => self.recv(arguments[0] as i32, arguments[1], arguments[2], arguments[3] as u32),
            "sendmsg" => self.sendmsg(arguments[0] as i32, arguments[1], arguments[2] as u32),
            "sendmmsg" => self.sendmmsg(
                arguments[0] as i32,
                arguments[1],
                arguments[2] as u32,
                arguments[3] as u32,
            ),
            "recvmmsg" => self.recvmmsg(
                arguments[0] as i32,
                arguments[1],
                arguments[2] as u32,
                arguments[3] as u32,
                arguments[4],
            ),
            "recvmsg" => self.recvmsg(arguments[0] as i32, arguments[1], arguments[2] as u32),
            "setsockopt" => self.setsockopt(
                arguments[0] as i32,
                arguments[1] as i32,
                arguments[2] as i32,
                arguments[3],
                arguments[4] as u32,
            ),
            "getsockopt" => self.getsockopt(
                arguments[0] as i32,
                arguments[1] as i32,
                arguments[2] as i32,
                arguments[3],
                arguments[4],
            ),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }
}
