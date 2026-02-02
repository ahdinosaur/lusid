mod arch;
mod hardware;
mod id;
mod os;
mod user;

pub use crate::arch::*;
pub use crate::hardware::*;
pub use crate::id::*;
pub use crate::os::*;
pub use crate::user::*;

pub struct System {
    pub hostname: Hostname,
    pub arch: Arch,
    pub os: Os,
    pub user: User,
}
