use crate::config::target::Target;
use crate::probe::MasterProbe;

pub struct Session {
    pub target: Target,
    pub probe: MasterProbe,
}

impl Session {
    /// Open a new session with a given debug target
    pub fn new(target: Target, probe: MasterProbe) -> Self {
        Self { target, probe }
    }
}

unsafe impl Send for Session {}