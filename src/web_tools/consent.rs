//! 私有/保留地址的用户同意通道(2026-11-08 负责人令):任何落在公网
//! 地址策略之外的目标——环回、RFC1918、link-local、TUN 代理 fake-IP
//! 段等——不再硬拒,而是弹出权限框让用户选择放行,并提供"不再询问"
//! (按主机记忆)。不枚举豁免任何网段:虚拟网卡环境用什么段是用户
//! 环境的自由,枚举是打地鼠(负责人原话),一律走同一同意门。
//!
//! 机制复用 `ask_user` 验证过的管线(`interaction::UserAsker`,前端
//! Some / headless None 自动降级);记忆存控制面 `web-consents.json`
//! (按主机,先入先出,上限 256)。guest/WASM 路径不经此通道,围栏
//! 语义不变(见 wasm-net 侧注释)。
use super::*;
use crate::interaction::{AskAnswer, AskOption, AskQuestion, AskQuestionPresentation};
use std::net::{IpAddr, ToSocketAddrs};

/// 弹框展示的地址数上限(解析结果可能多条,展示截断不影响放行集合;
/// 记忆容量 256 由控制面 save_web_consent 统一执行)。
const DISPLAY_ADDRESSES: usize = 3;

impl WebService {
    /// 所有出网请求的统一通道:首轮零同意集;地址策略拦截(`blocked`)
    /// 时解析目标 → 记忆/询问(挂 run_cancel 级取消令牌,不受 30s 请求
    /// deadline 掐断)→ 带同意集重试**一次**。重试仍拦(解析漂移)则
    /// 如实报错,绝不循环。
    pub(super) fn consent_execute(
        &self,
        request: HttpRequest,
        req_cancel: &CancelToken,
        run_cancel: &CancelToken,
    ) -> Result<HttpResponse, WebError> {
        match self.transport().execute(&request, req_cancel) {
            Err(error) if error.code == "blocked" => {
                self.consent_retry(request, req_cancel, run_cancel)
            }
            other => other,
        }
    }

    fn consent_retry(
        &self,
        mut request: HttpRequest,
        req_cancel: &CancelToken,
        run_cancel: &CancelToken,
    ) -> Result<HttpResponse, WebError> {
        check(req_cancel)?;
        let host = request_host(&request.url).ok_or_else(|| WebError::new("blocked"))?;
        let addresses = self
            .resolve(&host)
            .ok_or_else(|| WebError::new("blocked"))?;
        let private: Vec<IpAddr> = addresses
            .iter()
            .copied()
            .filter(|ip| !clat_wasm_net::dns_authority::address_is_public(*ip))
            .collect();
        // 解析全公网却仍被拦(解析竞态/瞬态):如实报 blocked,不循环。
        if private.is_empty() {
            return Err(WebError::new("blocked"));
        }
        self.demand_consent(&host, &private, run_cancel)?;
        // 同意集 = 该主机全部解析地址(公网地址本就能过,一并带入无害,
        // 且避免重试时解析顺序差异导致的漏配)。
        request.consented = addresses;
        self.transport().execute(&request, req_cancel)
    }

    /// 记忆命中直接放行;否则经 asker 弹框。选项三态:allow(本次)/
    /// always(记住,不再询问)/ deny。headless 无 asker → fail-closed。
    fn demand_consent(
        &self,
        host: &str,
        private: &[IpAddr],
        run_cancel: &CancelToken,
    ) -> Result<(), WebError> {
        let host = host.to_ascii_lowercase();
        if self
            .control
            .web_consents()
            .unwrap_or_default()
            .iter()
            .any(|remembered| remembered == &host)
        {
            return Ok(());
        }
        let Some(asker) = self.asker.asker() else {
            return Err(private_denied(
                &host,
                private,
                "no interactive user session",
            ));
        };
        let answer = asker.ask(consent_question(host.as_str(), private), run_cancel);
        match answer {
            AskAnswer::Selected(label) if label == "always" => {
                self.control
                    .save_web_consent(&host)
                    .map_err(|_| WebError::new("settings_failed"))?;
                Ok(())
            }
            AskAnswer::Selected(label) if label == "allow" => Ok(()),
            _ => Err(private_denied(&host, private, "denied")),
        }
    }
}

fn private_denied(host: &str, private: &[IpAddr], why: &str) -> WebError {
    WebError::new("private_denied").detail(format!(
        "{host} resolves to {} ({why})",
        display_addresses(private)
    ))
}

fn consent_question(host: &str, private: &[IpAddr]) -> AskQuestion {
    let address = display_addresses(private);
    AskQuestion {
        question: format!("Allow web access to {host} ({address})?"),
        options: vec![
            AskOption {
                label: "allow".into(),
                description: None,
            },
            AskOption {
                label: "always".into(),
                description: None,
            },
            AskOption {
                label: "deny".into(),
                description: None,
            },
        ],
        allow_custom: false,
        presentation: Some(AskQuestionPresentation {
            question: UiMessage::new("webtool.consent_question")
                .arg("host", host)
                .arg("address", address),
            options: vec![
                UiMessage::new("webtool.consent_allow"),
                UiMessage::new("webtool.consent_always"),
                UiMessage::new("webtool.consent_deny"),
            ],
        }),
    }
}

fn display_addresses(private: &[IpAddr]) -> String {
    private
        .iter()
        .take(DISPLAY_ADDRESSES)
        .map(IpAddr::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn request_host(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_owned)
}

impl WebService {
    /// 系统解析器口径(与传输层 DNS 同源):数字字面量直接成地址,
    /// 域名走 getaddrinfo;测试经 resolve_hook 注入固定映射。失败/为空
    /// 由调用方按 blocked 处理。
    pub(super) fn resolve(&self, host: &str) -> Option<Vec<IpAddr>> {
        if let Some(hook) = self.resolve_hook.lock().unwrap().as_ref() {
            return hook(host);
        }
        resolve_system(host)
    }
}

fn resolve_system(host: &str) -> Option<Vec<IpAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(vec![ip]);
    }
    let mut addresses: Vec<IpAddr> = (host, 0)
        .to_socket_addrs()
        .ok()?
        .map(|socket| socket.ip())
        .collect();
    addresses.dedup();
    (!addresses.is_empty()).then_some(addresses)
}
