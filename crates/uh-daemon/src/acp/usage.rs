//! What the session has used, for the panel's context gauge: the context
//! against the model's window and the automatic compaction's threshold,
//! what the conversation cost, and whether a compaction runs. Published as
//! a session-scoped `session_info_update` (`_meta.unharnessed.usage`) when
//! the session opens, after each message the loop settles, around a
//! compaction and after a picker change, whenever it changed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use uh_core::session_engine::engine::SessionEngine;

use super::meta::{
    unharnessed_meta, UnharnessedEventPhase, UnharnessedSessionMeta, UnharnessedUsageMeta,
};
use super::producer::UpdateProducer;
use super::types::AcpSessionUpdate;

pub struct UsageReporter {
    engine: Arc<SessionEngine>,
    /// The session's live model (the slot a picker switch updates).
    model: Arc<Mutex<Option<uh_types::ai::Model>>>,
    producer: Arc<UpdateProducer>,
    /// The last report published.
    last: Mutex<Option<UnharnessedUsageMeta>>,
    compacting: AtomicBool,
}

impl UsageReporter {
    pub fn new(
        engine: Arc<SessionEngine>,
        model: Arc<Mutex<Option<uh_types::ai::Model>>>,
        producer: Arc<UpdateProducer>,
    ) -> Arc<UsageReporter> {
        Arc::new(UsageReporter {
            engine,
            model,
            producer,
            last: Mutex::new(None),
            compacting: AtomicBool::new(false),
        })
    }

    /// A compaction starts or ends.
    pub async fn compacting(&self, running: bool) {
        self.compacting.store(running, Ordering::SeqCst);
        self.changed().await;
    }

    /// Publish the report if it changed. It reads the persisted session,
    /// not the agent's state: the loop's event listeners call it.
    pub async fn changed(&self) {
        let mut last = self.last.lock().await;
        let Some(model) = self.model.lock().await.clone() else {
            return;
        };
        let usage = self.engine.session.usage(&model).await;
        let report = UnharnessedUsageMeta {
            tokens: usage.tokens,
            window: usage.context_window,
            compact_at: usage.compact_at,
            cost: usage.cost,
            billed: model.provider == "openrouter",
            cache_reusable: usage.cache_reusable,
            cache_reused: usage.cache_reused,
            cache_saved: usage.cache_saved,
            compacting: self.compacting.load(Ordering::SeqCst),
        };
        if last.as_ref() == Some(&report) {
            return;
        }
        let update = AcpSessionUpdate::SessionInfoUpdate {
            meta: unharnessed_meta(&UnharnessedSessionMeta {
                usage: Some(report.clone()),
                ..Default::default()
            }),
        };
        self.producer
            .publish(&update, 0, UnharnessedEventPhase::Event, None)
            .await;
        *last = Some(report);
    }
}
