//! Concrete transport mechanisms, named by technology. Today: [`unix`] — the Unix-domain socket carrying
//! framed submits + acks + the handshake, `SCM_RIGHTS` fd transfer, the render-node ioctl, and the futex
//! doorbell. A future shared-memory command ring would be a sibling module here.

pub mod unix;
