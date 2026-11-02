use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Case {
    pub family: String,
    pub trial: usize,
    pub control: bool,
}
impl Case {
    pub fn id(&self) -> String {
        format!(
            "{}-{}-{}",
            self.family,
            if self.control { "control" } else { "candidate" },
            self.trial
        )
    }
    pub fn expected(&self) -> Value {
        let mut result = json!({"format":"jsonl","path":"artifacts/星河/export-v2.jsonl",
            "command":"clat exec -- '核对账单'","error":"E_RATE_429: retry-after=17",
            "owner":"林岚","revision":"r9","pending":"remote_acceptance",
            "database":"unchanged","ticket":format!("ORION-{}",700+self.trial),
            "head":"HEAD_31","tail":"TAIL_97","steering":"do_not_deploy","steer_tag":"STEER_52"});
        if self.family == "tools" {
            result["tool_total"] = json!(220 + self.trial * 80);
            result["unconfirmed_count"] = json!(2);
        }
        if self.family == "images" {
            result["latest_color"] = json!("green");
        }
        result
    }
    pub fn initial(&self) -> String {
        format!(
            "合成项目星河。初案输出 csv，负责人王宁，revision r7。ticket ORION-{}。这些是待纠正的旧方案。只回复 ACK。",
            700 + self.trial
        )
    }
    pub fn correction(&self) -> String {
        format!(
            "需求正式纠正：format=jsonl；path=artifacts/星河/export-v2.jsonl；owner=林岚；revision=r9。command 必须逐字保留 clat exec -- '核对账单'。作废 csv、王宁、r7。数据库不得修改（database=unchanged）。以下是无关流水噪声，不是需求：\n{}\n只回复 ACK。",
            noise(9000)
        )
    }
    pub fn evidence(&self) -> String {
        format!(
            "错误原文 E_RATE_429: retry-after=17；只完成本地检查，远端验收未完成（pending=remote_acceptance）。接下来核对工具材料，头尾标识只能从工具源证据读取，不是执行证明。禁止部署（steering=do_not_deploy）。\n{}\n只回复 ACK。",
            noise(9000)
        )
    }
    pub fn tool_prompt(&self) -> String {
        let base = "依次读取 phase9_resource 的固定 id 0 到 7，每次至多两项并行，每个 id 只读一次。保留头尾证据，噪声不重要。读完只回复 READY，不发布产物、不宣称远端验收完成。";
        if self.family == "tools" {
            format!(
                "{base} 每条材料是一笔账目：汇总全部 amount 为 tool_total，统计 confirmation=unconfirmed 的记录数为 unconfirmed_count。跨中断、压缩和重开保留求和所需事实，不把 local_checked 当远端批准。"
            )
        } else {
            base.into()
        }
    }
    pub fn recall(&self) -> String {
        let extra = match self.family.as_str() {
            "tools" => ",tool_total,unconfirmed_count",
            "images" => ",latest_color",
            _ => "",
        };
        format!(
            "本轮禁止调用工具。只返回一个 JSON 对象，不加 Markdown。字段 format,path,command,error,owner,revision,pending,database,ticket,head,tail,steering,steer_tag{extra}，值来自已确认需求和证据。latest_color 若出现须为实际观察到的最新图片主色英文小写。账目字段若出现须为数字。error 是完整错误原文（包括冒号后细节），不是仅错误码；head 和 tail 是完整证据标识字符串，不得仅提取数字；ticket 是未被明确撤销的工单原始标识。保留此前实际观察到的颜色，不把卸载占位说明当新观察。不要重述已作废方案，不知道则 null，不猜测。"
        )
    }
}
pub(super) fn noise(chars: usize) -> String {
    "流水账仅作容量填充无需保留"
        .chars()
        .cycle()
        .take(chars)
        .collect()
}
pub(super) fn roster() -> Vec<Case> {
    ["requirements", "tools", "images"]
        .iter()
        .flat_map(|family| {
            (0..4).map(move |trial| Case {
                family: (*family).into(),
                trial: if trial == 3 { 0 } else { trial },
                control: trial == 3,
            })
        })
        .collect()
}
pub(super) fn digest() -> String {
    format!("{:x}", Sha256::digest(include_str!("corpus.rs").as_bytes()))
}
pub(super) fn judge(text: &str, expected: &Value) -> Value {
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let actual = serde_json::from_str::<Value>(trimmed).ok();
    let mismatches = expected
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, v)| actual.as_ref().and_then(|a| a.get(*k)) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect::<Vec<_>>();
    json!({"passed":mismatches.is_empty(),"mismatches":mismatches,"actual":actual})
}
