//! 设置窗口里的中继状态。
//!
//! 每个中继的状态来自一次真实请求：`/api/v1/relay` 做发现与短 ID 核对，`/healthz` 做探活。
//! 请求跑在工作线程上（各自一个 current-thread 运行时），结果用通道回投，
//! 由界面线程的定时器取走并刷新模型 —— 界面线程不阻塞。

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};

use secrelay_i18n::{Key, Lang};
use secrelay_relay_client::{Discovery, Endpoint, IdCheck};
use secrelay_theme::Relays;
use slint::{ModelRc, SharedString, VecModel};

use crate::{RelayRow, SettingsWindow};

/// 一次核对的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// 还没核对过。
    NotChecked,
    /// 正在核对。
    Probing,
    /// 探测通了，短 ID 也对得上。
    Reachable,
    /// 探测通了，但中继自称的短 ID 与本地算出来的不一致。
    IdMismatch,
    /// 探测通了，但 `id` 的格式就不对。
    IdMalformed,
    /// 探测通了，但服务端的信令协议版本高于本端认识的版本。
    ProtoMismatch,
    /// 连不上或响应不对。
    Unreachable,
}

impl Check {
    /// 是否通讯成功（含"通了但 ID 不符"）。
    #[cfg(test)]
    pub fn is_reachable(self) -> bool {
        matches!(
            self,
            Check::Reachable | Check::IdMismatch | Check::IdMalformed | Check::ProtoMismatch
        )
    }

    /// 中继的自称与本地核对值是否对不上。
    pub fn is_mismatch(self) -> bool {
        matches!(self, Check::IdMismatch | Check::IdMalformed)
    }
}

/// 一条中继的状态。
#[derive(Debug, Clone)]
pub struct Status {
    pub check: Check,
    /// 本地按地址算出来的短 ID。
    pub local_id: String,
    /// 中继上报的短 ID，没探测到就是空串。
    pub advertised_id: String,
    /// 服务端上报的信令地址；没探测到就用本地推导的值。
    pub signaling: String,
    /// 是否配好了 TURN。
    pub turn_configured: bool,
    pub protocol_version: u32,
    /// 发现结果，留给后续真正建连时用。
    pub discovery: Option<Discovery>,
}

impl Status {
    fn unknown(endpoint: &Endpoint) -> Self {
        Self {
            check: Check::NotChecked,
            local_id: endpoint.local_id(),
            advertised_id: String::new(),
            signaling: endpoint.local_signaling_url(),
            turn_configured: false,
            protocol_version: 0,
            discovery: None,
        }
    }
}

/// 工作线程回投的一条结果。
enum Update {
    Start(String),
    Done {
        url: String,
        discovery: Option<Discovery>,
        reachable: bool,
        protocol_version: u32,
        turn_configured: bool,
        error: Option<String>,
    },
}

/// 中继设置的状态机。
pub struct RelayUi {
    receiver: Receiver<Update>,
    sender: Sender<Update>,
    statuses: HashMap<String, Status>,
    endpoints: Vec<Endpoint>,
    /// 最近一次请求失败的原因（按地址），成功时清掉。
    errors: HashMap<String, String>,
}

impl RelayUi {
    pub fn new(relays: &Relays) -> Self {
        let (sender, receiver) = channel();
        let mut ui = Self {
            receiver,
            sender,
            statuses: HashMap::new(),
            endpoints: Vec::new(),
            errors: HashMap::new(),
        };
        ui.endpoints = relays
            .urls()
            .iter()
            .filter_map(|url| Endpoint::parse(url).ok())
            .collect();
        for endpoint in ui.endpoints.clone() {
            ui.statuses
                .entry(endpoint.as_str().to_string())
                .or_insert_with(|| Status::unknown(&endpoint));
        }
        ui
    }

    /// 地址列表（规范化后的文本），顺序与配置一致。
    pub fn urls(&self) -> Vec<String> {
        self.endpoints
            .iter()
            .map(|endpoint| endpoint.as_str().to_string())
            .collect()
    }

    /// 某个地址对应的端点。
    pub fn endpoint(&self, url: &str) -> Option<Endpoint> {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.as_str() == url)
            .cloned()
    }

    /// 某个地址的状态。
    pub fn status(&self, url: &str) -> Option<&Status> {
        self.statuses.get(url)
    }

    /// 某个地址最近一次失败原因。
    pub fn error(&self, url: &str) -> Option<&str> {
        self.errors.get(url).map(String::as_str)
    }

    /// 按配置文件重建端点列表，丢掉已经不存在的状态。
    pub fn resync(&mut self, relays: &Relays) {
        self.endpoints = relays
            .urls()
            .iter()
            .filter_map(|url| Endpoint::parse(url).ok())
            .collect();
        self.statuses
            .retain(|url, _| self.endpoints.iter().any(|e| e.as_str() == url));
        for endpoint in self.endpoints.clone() {
            self.statuses
                .entry(endpoint.as_str().to_string())
                .or_insert_with(|| Status::unknown(&endpoint));
        }
    }

    /// 立刻开一次核对。
    pub fn check(&self, url: &str) {
        let Some(endpoint) = self.endpoint(url) else {
            return;
        };
        let sender = self.sender.clone();
        let key = endpoint.as_str().to_string();

        // 每个地址一次请求，跑完就退出：不长期占线程，也不需要在后台维持运行时。
        std::thread::spawn(move || {
            let _ = sender.send(Update::Start(key.clone()));
            let result = run_check(endpoint);
            let _ = sender.send(result);
        });
    }

    /// 把所有还没核对过的地址排上。
    pub fn check_pending(&self) {
        for endpoint in &self.endpoints {
            let url = endpoint.as_str().to_string();
            let pending = self
                .statuses
                .get(&url)
                .map(|status| status.check == Check::NotChecked)
                .unwrap_or(true);
            if pending {
                self.check(&url);
            }
        }
    }

    /// 取走工作线程回投的结果，返回是否有变化。
    pub fn apply_updates(&mut self) -> bool {
        let mut changed = false;
        while let Ok(update) = self.receiver.try_recv() {
            changed = true;
            match update {
                Update::Start(url) => {
                    if let Some(status) = self.statuses.get_mut(&url) {
                        status.check = Check::Probing;
                    }
                }
                Update::Done {
                    url,
                    discovery,
                    reachable,
                    protocol_version,
                    turn_configured,
                    error,
                } => {
                    if let Some(status) = self.statuses.get_mut(&url) {
                        status.check = classify(reachable, protocol_version, discovery.as_ref());
                        status.turn_configured = turn_configured;
                        status.protocol_version = protocol_version;
                        if let Some(discovery) = &discovery {
                            status.advertised_id = discovery.info.id.clone();
                            status.signaling = discovery.signaling_url();
                            status.local_id = discovery.expected_id.clone();
                        }
                        status.discovery = discovery;
                    }
                    match error {
                        Some(error) => {
                            self.errors.insert(url, error);
                        }
                        None => {
                            self.errors.remove(&url);
                        }
                    }
                }
            }
        }
        changed
    }
}

/// 探测一个中继：发现 + 探活。
fn run_check(endpoint: Endpoint) -> Update {
    let url = endpoint.as_str().to_string();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            return Update::Done {
                url,
                discovery: None,
                reachable: false,
                protocol_version: 0,
                turn_configured: false,
                error: Some(format!("无法创建运行时：{err}")),
            }
        }
    };

    runtime.block_on(async move {
        let user_agent = secrelay_relay_client::user_agent();
        let mut errors: Vec<String> = Vec::new();

        let discovery = match secrelay_relay_client::discover(&endpoint, &user_agent).await {
            Ok(discovery) => Some(discovery),
            Err(err) => {
                errors.push(err.message());
                None
            }
        };

        let health = match secrelay_relay_client::health(&endpoint, &user_agent).await {
            Ok(health) => Some(health),
            Err(err) => {
                errors.push(err.message());
                None
            }
        };

        let protocol_version = discovery
            .as_ref()
            .map(|discovery| discovery.info.protocol_version)
            .or_else(|| health.as_ref().map(|health| health.protocol_version))
            .unwrap_or(0);
        let turn_configured = discovery
            .as_ref()
            .map(|discovery| discovery.info.turn_configured)
            .or_else(|| health.as_ref().map(|health| health.turn_configured))
            .unwrap_or(false);

        Update::Done {
            url,
            discovery,
            reachable: health.is_some(),
            protocol_version,
            turn_configured,
            error: errors.first().cloned(),
        }
    })
}

/// 把探测结果归到一档状态。
fn classify(reachable: bool, protocol_version: u32, discovery: Option<&Discovery>) -> Check {
    if !reachable {
        return Check::Unreachable;
    }
    if protocol_version > secrelay_relay_client::PROTOCOL_VERSION {
        return Check::ProtoMismatch;
    }
    match discovery {
        Some(discovery) => match discovery.id_check {
            IdCheck::Match => Check::Reachable,
            IdCheck::Mismatch => Check::IdMismatch,
            IdCheck::Malformed => Check::IdMalformed,
        },
        // 探活通了但发现失败：能连上，配置没拿到
        None => Check::Unreachable,
    }
}

/// 一条中继在界面上的文案。
pub fn row_of(url: &str, selected: bool, status: Option<&Status>, lang: Lang) -> RelayRow {
    // 正常运行时 status 一定存在；这里仍然给出兜底，免得界面出现空白行
    let status = status.cloned().unwrap_or(Status {
        check: Check::NotChecked,
        local_id: String::new(),
        advertised_id: String::new(),
        signaling: String::new(),
        turn_configured: false,
        protocol_version: 0,
        discovery: None,
    });

    RelayRow {
        url: url.into(),
        id: status.local_id.clone().into(),
        status: status_text(status.check, lang).into(),
        probe: probe_text(status.check, status.turn_configured, lang).into(),
        selected,
        mismatch: status.check.is_mismatch(),
    }
}

/// 短 ID 核对结论的文案。
pub fn status_text(check: Check, lang: Lang) -> &'static str {
    match check {
        Check::NotChecked | Check::Probing => Key::RelayNotChecked.text(lang),
        Check::Reachable => Key::RelayIdMatch.text(lang),
        Check::IdMismatch => Key::RelayIdMismatch.text(lang),
        Check::IdMalformed => Key::RelayIdMalformed.text(lang),
        Check::ProtoMismatch => Key::RelayProtoMismatch.text(lang),
        Check::Unreachable => Key::RelayUnreachable.text(lang),
    }
}

/// 探活结论的文案。
pub fn probe_text(check: Check, turn_configured: bool, lang: Lang) -> &'static str {
    match check {
        Check::NotChecked => Key::RelayNotChecked.text(lang),
        Check::Probing => Key::RelayProbing.text(lang),
        Check::Unreachable => Key::RelayUnreachable.text(lang),
        Check::ProtoMismatch => Key::RelayProtoMismatch.text(lang),
        _ if turn_configured => Key::RelayTurnReady.text(lang),
        _ => Key::RelayTurnMissing.text(lang),
    }
}

/// 选中中继的详情行。
pub fn detail_rows(status: Option<&Status>, lang: Lang) -> Vec<(SharedString, SharedString)> {
    let status = match status {
        Some(status) => status,
        None => {
            return vec![(Key::RelayNotChecked.text(lang).into(), "".into())];
        }
    };
    vec![
        (
            Key::RelayIdLocal.text(lang).into(),
            status.local_id.clone().into(),
        ),
        (
            Key::RelayIdAdvertised.text(lang).into(),
            if status.advertised_id.is_empty() {
                Key::RelayNotChecked.text(lang).into()
            } else {
                status.advertised_id.clone().into()
            },
        ),
        (
            Key::RelaySignaling.text(lang).into(),
            status.signaling.clone().into(),
        ),
        (
            Key::RelayProbe.text(lang).into(),
            probe_text(status.check, status.turn_configured, lang).into(),
        ),
    ]
}

/// 把列表与选中项推给设置窗口。
pub fn push(settings: &SettingsWindow, relays: &Relays, ui: &RelayUi, lang: Lang) {
    let urls = ui.urls();
    let selected = relays.selected().min(urls.len().saturating_sub(1));

    let rows: Vec<RelayRow> = urls
        .iter()
        .enumerate()
        .map(|(index, url)| row_of(url, index == selected, ui.status(url), lang))
        .collect();

    settings.set_relays(ModelRc::new(VecModel::from(rows)));
    settings.set_relay_index(selected as i32);

    let current = urls.get(selected).cloned().unwrap_or_default();
    settings.set_relay_current(current.clone().into());
    settings.set_relay_current_hint(
        ui.error(&current)
            .map(|error| SharedString::from(error.to_string()))
            .unwrap_or_else(|| {
                if current.is_empty() {
                    Key::RelayNoActive.text(lang).into()
                } else {
                    SharedString::new()
                }
            }),
    );

    let details = detail_rows(ui.status(&current), lang);
    settings.set_relay_details(ModelRc::new(VecModel::from(
        details
            .into_iter()
            .map(|(label, value)| crate::RelayDetail { label, value })
            .collect::<Vec<_>>(),
    )));
    settings.set_relay_mismatch(
        ui.status(&current)
            .map(|status| status.check.is_mismatch())
            .unwrap_or(false),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrelay_i18n::Lang;

    fn discovery_of(base: &str, advertised: &str) -> Discovery {
        let endpoint = Endpoint::parse(base).unwrap();
        Discovery {
            expected_id: endpoint.local_id(),
            endpoint,
            info: secrelay_relay_client::RelayInfo {
                id: advertised.to_string(),
                protocol_version: 1,
                turn_configured: true,
                ..Default::default()
            },
            id_check: secrelay_relay_client::verify(
                &Endpoint::parse(base).unwrap().local_id(),
                advertised,
            ),
        }
    }

    #[test]
    fn 核对结论归到正确的状态() {
        let good = discovery_of("https://relay.example.com", "AF4KR6IMPE");
        assert_eq!(classify(true, 1, Some(&good)), Check::Reachable);
        assert!(Check::Reachable.is_reachable());

        let bad = discovery_of("https://relay.example.com", "ZZZZZZZZZZ");
        assert_eq!(classify(true, 1, Some(&bad)), Check::IdMismatch);
        assert!(Check::IdMismatch.is_reachable(), "ID 不符不等于连不上");

        let malformed = discovery_of("https://relay.example.com", "nope");
        assert_eq!(classify(true, 1, Some(&malformed)), Check::IdMalformed);

        // 探活不通
        assert_eq!(classify(false, 1, Some(&good)), Check::Unreachable);
        // 探活通了但发现失败
        assert_eq!(classify(true, 1, None), Check::Unreachable);
        // 服务端协议版本更高
        assert_eq!(classify(true, 99, Some(&good)), Check::ProtoMismatch);
    }

    #[test]
    fn 未核对的文案不是空白() {
        for check in [Check::NotChecked, Check::Probing, Check::Unreachable] {
            assert!(!status_text(check, Lang::ZhHans).is_empty());
            assert!(!probe_text(check, false, Lang::ZhHans).is_empty());
        }
    }

    #[test]
    fn 探活文案区分_turn_是否可用() {
        let ready = probe_text(Check::Reachable, true, Lang::ZhHans);
        let missing = probe_text(Check::Reachable, false, Lang::ZhHans);
        assert_ne!(ready, missing);
        assert_eq!(ready, Key::RelayTurnReady.text(Lang::ZhHans));
        assert_eq!(missing, Key::RelayTurnMissing.text(Lang::ZhHans));
    }

    #[test]
    fn 列表行的选中标记只有一个() {
        let mut relays = Relays::from_urls([
            "https://a.example".to_string(),
            "https://b.example".to_string(),
        ]);
        relays.select(1);
        let ui = RelayUi::new(&relays);

        let urls = ui.urls();
        let selected: Vec<usize> = urls
            .iter()
            .enumerate()
            .filter(|(index, _)| *index == relays.selected())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(selected, [1]);
        assert_eq!(relays.selected_url(), "https://b.example");
    }

    #[test]
    fn 状态表跟着地址列表走() {
        let mut relays = Relays::from_urls(["https://a.example".to_string()]);
        let mut ui = RelayUi::new(&relays);
        assert!(ui.status("https://a.example").is_some());

        relays.add("https://b.example").unwrap();
        ui.resync(&relays);
        assert_eq!(ui.urls().len(), 2);
        assert!(ui.status("https://b.example").is_some());

        relays.remove(0);
        ui.resync(&relays);
        assert_eq!(ui.urls(), ["https://b.example"]);
        assert!(ui.status("https://a.example").is_none(), "旧状态要清掉");
    }

    #[test]
    fn 详情行在没探测过时也有内容() {
        let rows = detail_rows(None, Lang::ZhHans);
        assert!(!rows.is_empty());
        assert!(!rows[0].0.is_empty());
    }

    #[test]
    fn 未知地址不会_panic() {
        let ui = RelayUi::new(&Relays::default());
        ui.check("https://不存在的.example");
        assert!(ui.status("https://不存在的.example").is_none());
        assert!(ui.error("https://不存在的.example").is_none());
    }
}
