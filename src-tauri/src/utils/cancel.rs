//! Token anulowania współdzielony między GUI (przycisk Anuluj), zadaniami
//! roboczymi i runnerem procesów (anulowanie zabija działający `adb`).

use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct CancelToken(CancellationToken);

impl CancelToken {
    pub fn new() -> Self {
        Self(CancellationToken::new())
    }

    pub fn cancel(&self) {
        self.0.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    /// Future kończący się w chwili anulowania (do `tokio::select!`).
    pub async fn cancelled(&self) {
        self.0.cancelled().await
    }
}
