//! The long-lived bus from the driver/subprocess side to the GTK thread. One
//! `Receiver` is drained by `App::pump_events`; everything else clones the `Sender`.
use crate::mongo::ConnectionId;

#[derive(Debug)]
pub enum Event {
    Connected {
        conn: ConnectionId,
        result: crate::mongo::Connected,
    },
    ConnectFailed {
        conn: ConnectionId,
        error: String,
    },
    Disconnected {
        conn: ConnectionId,
        reason: Option<String>,
    },
    /// Progress of a long operation (import/export/bulk), for the banner.
    Progress {
        op: uuid::Uuid,
        done: u64,
        total: Option<u64>,
        label: String,
    },
    OpDone {
        op: uuid::Uuid,
        result: Result<String, String>,
    },
}

pub type Sender = async_channel::Sender<Event>;
pub type Receiver = async_channel::Receiver<Event>;
