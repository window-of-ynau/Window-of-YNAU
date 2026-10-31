//! 并发闸门：按 **turn** 计数，不按 HTTP 请求计数（ADR-03 / ADR-12）。
//!
//! 两条规则同时生效：
//!
//! 1. 全局最多 `concurrency_limit` 个在途 turn（默认 50）：保护上游速率与我们的成本；
//! 2. 单用户同时只允许 1 个在途 turn：防止一个客户端开一堆流把闸门吃光。
//!
//! 排队语义取自 `docs/02` §8.1「超出排队 ≤ 5s 后 429」：全局档位允许短暂等待，
//! 单用户重复占用则**立刻**拒绝（等也不会有意义，客户端该先取消旧 turn）。
//!
//! 占用随 [`TurnPermit`] 一起释放（`Drop`）：客户端断开、上游报错、正常结束都会走到这里，
//! 所以不会出现"连接还在、闸门名额泄漏"。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

/// 拒绝原因：对外统一 `429 CONCURRENCY_LIMIT`，措辞与 `Retry-After` 由这里给。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateRejection {
    /// 同一个 turn 已经在途（重试/重复提交）
    TurnInFlight,
    /// 该用户还有别的 turn 在途
    UserInFlight,
    /// 全局档位已满且等待超时
    GlobalBusy,
}

impl GateRejection {
    pub fn message(self) -> String {
        match self {
            Self::TurnInFlight => "同一 turn 已有在途请求，请先取消它或等它结束".to_owned(),
            Self::UserInFlight => "该账号同时只允许 1 个在途对话，请等上一轮结束".to_owned(),
            Self::GlobalBusy => "服务端并发已满，请稍后重试".to_owned(),
        }
    }

    /// `Retry-After`（秒）。三条原因都建议 1s 后再试。
    pub fn retry_after_secs(self) -> u64 {
        1
    }
}

#[derive(Debug, Default)]
struct GateState {
    /// turn_id → 用户（判断"同一 turn 重复提交"）
    turns: HashMap<String, Uuid>,
    /// 用户 → 在途 turn_id（判断"单用户 1 个"）
    per_user: HashMap<Uuid, String>,
}

/// 进程内闸门（ADR-12：1.0 单实例，不需要分布式锁）。
#[derive(Debug)]
pub struct TurnGate {
    semaphore: Arc<Semaphore>,
    state: Arc<Mutex<GateState>>,
    queue: Duration,
    limit: usize,
}

impl TurnGate {
    pub fn new(limit: usize, queue: Duration) -> Self {
        let limit = limit.max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(limit)),
            state: Arc::new(Mutex::new(GateState::default())),
            queue,
            limit,
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// 当前在途 turn 数（指标/测试用）。
    pub fn in_flight(&self) -> usize {
        self.lock().turns.len()
    }

    /// 申请一个 turn 名额。拿到的 [`TurnPermit`] 活着就意味着"这个 turn 在途"。
    pub async fn acquire(&self, user_id: Uuid, turn_id: &str) -> Result<TurnPermit, GateRejection> {
        let permit = if self.queue.is_zero() {
            self.semaphore
                .clone()
                .try_acquire_owned()
                .map_err(|_| GateRejection::GlobalBusy)?
        } else {
            match tokio::time::timeout(self.queue, self.semaphore.clone().acquire_owned()).await {
                Ok(Ok(permit)) => permit,
                // 信号量被关闭 / 排队超时：都按"档位已满"处理
                Ok(Err(_)) | Err(_) => return Err(GateRejection::GlobalBusy),
            }
        };

        // 检查与占位必须在同一把锁里完成，否则同一 turn 的并发请求会双双通过
        {
            let mut state = self.lock();
            if state.turns.contains_key(turn_id) {
                return Err(GateRejection::TurnInFlight);
            }
            if state.per_user.contains_key(&user_id) {
                return Err(GateRejection::UserInFlight);
            }
            state.turns.insert(turn_id.to_owned(), user_id);
            state.per_user.insert(user_id, turn_id.to_owned());
        }

        Ok(TurnPermit {
            state: Arc::clone(&self.state),
            turn_id: turn_id.to_owned(),
            user_id,
            _global: permit,
        })
    }

    fn lock(&self) -> MutexGuard<'_, GateState> {
        // 闸门状态永远是"要么整块更新、要么没更新"，毒化也照样能用
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 在途 turn 的凭证：`Drop` 即释放（断开、报错、正常结束都会走这里）。
#[derive(Debug)]
pub struct TurnPermit {
    state: Arc<Mutex<GateState>>,
    turn_id: String,
    user_id: Uuid,
    _global: OwnedSemaphorePermit,
}

impl TurnPermit {
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub fn user_id(&self) -> Uuid {
        self.user_id
    }
}

impl Drop for TurnPermit {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.turns.remove(&self.turn_id);
        state.per_user.remove(&self.user_id);
        // OwnedSemaphorePermit 在这里随之释放
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_user() -> Uuid {
        Uuid::new_v4()
    }

    #[tokio::test]
    async fn second_request_for_the_same_turn_is_rejected() {
        let gate = TurnGate::new(50, Duration::ZERO);
        let alice = new_user();
        let _permit = gate.acquire(alice, "turn-1").await.expect("首次应通过");

        let rejection = gate
            .acquire(alice, "turn-1")
            .await
            .expect_err("同一 turn 必须拒绝");
        assert!(
            matches!(rejection, GateRejection::TurnInFlight),
            "{rejection:?}"
        );
        assert_eq!(gate.in_flight(), 1);
    }

    #[tokio::test]
    async fn one_in_flight_turn_per_user() {
        let gate = TurnGate::new(50, Duration::ZERO);
        let alice = new_user();
        let _permit = gate.acquire(alice, "turn-1").await.expect("首次应通过");

        let rejection = gate
            .acquire(alice, "turn-2")
            .await
            .expect_err("同一用户的第二个 turn 必须拒绝");
        assert!(
            matches!(rejection, GateRejection::UserInFlight),
            "{rejection:?}"
        );

        // 别的用户不受影响
        gate.acquire(new_user(), "turn-3")
            .await
            .expect("其他用户应通过");
    }

    #[tokio::test]
    async fn global_limit_is_enforced_without_queueing() {
        let gate = TurnGate::new(1, Duration::ZERO);
        let _permit = gate
            .acquire(new_user(), "turn-1")
            .await
            .expect("首个应通过");

        let rejection = gate
            .acquire(new_user(), "turn-2")
            .await
            .expect_err("全局档位满了必须拒绝");
        assert!(
            matches!(rejection, GateRejection::GlobalBusy),
            "{rejection:?}"
        );
    }

    #[tokio::test]
    async fn permit_release_frees_both_slots() {
        let gate = TurnGate::new(1, Duration::ZERO);
        let alice = new_user();
        {
            let _permit = gate.acquire(alice, "turn-1").await.expect("首次应通过");
            assert_eq!(gate.in_flight(), 1);
        }
        assert_eq!(gate.in_flight(), 0, "permit 释放后名额必须归还");
        // 同一个 turn_id、同一个用户都应该能再来一次（重试场景）
        gate.acquire(alice, "turn-1")
            .await
            .expect("释放后应能重新申请");
    }

    #[tokio::test]
    async fn queue_waits_then_succeeds_when_slot_frees() {
        let gate = Arc::new(TurnGate::new(1, Duration::from_millis(200)));
        let permit = gate
            .acquire(new_user(), "turn-1")
            .await
            .expect("首个应通过");

        let gate_clone = Arc::clone(&gate);
        let waiter = tokio::spawn(async move { gate_clone.acquire(new_user(), "turn-2").await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(permit);
        let acquired = waiter.await.expect("任务不应 panic");
        assert!(acquired.is_ok(), "排队者应在名额释放后拿到档位");
    }

    #[tokio::test]
    async fn queue_timeout_becomes_global_busy() {
        let gate = TurnGate::new(1, Duration::from_millis(30));
        let _permit = gate
            .acquire(new_user(), "turn-1")
            .await
            .expect("首个应通过");
        let rejection = gate
            .acquire(new_user(), "turn-2")
            .await
            .expect_err("排队超时必须拒绝");
        assert!(
            matches!(rejection, GateRejection::GlobalBusy),
            "{rejection:?}"
        );
    }

    #[test]
    fn rejections_have_actionable_messages() {
        for rejection in [
            GateRejection::TurnInFlight,
            GateRejection::UserInFlight,
            GateRejection::GlobalBusy,
        ] {
            assert!(!rejection.message().is_empty());
            assert_eq!(rejection.retry_after_secs(), 1);
        }
    }
}
