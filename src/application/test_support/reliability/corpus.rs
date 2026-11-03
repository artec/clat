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
    pub(super) fn variant(&self, text: String) -> String {
        if self.trial < 10 {
            return text;
        }
        text.replace(
            "artifacts/星河/export-v2.jsonl",
            &format!(
                "artifacts/合成{}/out-{}-final.jsonl",
                self.trial,
                self.trial + 4
            ),
        )
        .replace("林岚", ["周禾", "许澄", "Mira"][self.trial % 3])
        .replace("revision=r9", &format!("revision=r{}", self.trial + 11))
        .replace(
            "retry-after=17",
            &format!("retry-after={}", self.trial + 17),
        )
        .replace("HEAD_31", &format!("HEAD_{}", self.trial + 31))
        .replace("TAIL_97", &format!("TAIL_{}", self.trial + 97))
    }

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
            "合成项目星河。初案输出 csv，负责人王宁，revision r7。ticket ORION-{}。这些是待纠正的旧方案。{}只回复 ACK。",
            700 + self.trial,
            self.scope_note("initial")
        )
    }
    pub fn correction(&self) -> String {
        format!(
            "需求正式纠正：format=jsonl；path=artifacts/星河/export-v2.jsonl；owner=林岚；revision=r9。command 必须逐字保留 clat exec -- '核对账单'。作废 csv、王宁、r7。数据库不得修改（database=unchanged）。{}以下是无关流水噪声，不是需求：\n{}\n只回复 ACK。",
            self.scope_note("correction"),
            noise(9000)
        )
    }
    pub fn evidence(&self) -> String {
        format!(
            "错误原文 E_RATE_429: retry-after=17；只完成本地检查，远端验收未完成（pending=remote_acceptance）。接下来核对工具材料，头尾标识只能从工具源证据读取，不是执行证明。禁止部署（steering=do_not_deploy）。{}\n{}\n只回复 ACK。",
            self.scope_note("evidence"),
            noise(9000)
        )
    }
    pub fn tool_prompt(&self) -> String {
        let order = match self.trial {
            21 => "按顺序7、0、6、1、5、2、4、3读取",
            22 => "按顺序2、6、0、4、1、7、3、5读取",
            30 => "按顺序3、0、7、4、1、6、2、5读取",
            31 => "按顺序5、1、6、2、7、3、0、4读取",
            32 => "按顺序4、7、2、5、0、3、6、1读取",
            _ => "依次读取",
        };
        let base = format!(
            "{order} phase9_resource 的固定 id 0 到 7，每次至多两项并行，每个 id 只读一次。保留头尾证据，噪声不重要。读完只回复 READY，不发布产物、不宣称远端验收完成。"
        );
        if self.family == "tools" {
            format!(
                "{base} 每条材料是一笔账目：汇总全部 amount 为 tool_total，统计 confirmation=unconfirmed 的记录数为 unconfirmed_count。跨中断、压缩和重开保留求和所需事实，不把 local_checked 当远端批准。"
            )
        } else {
            base
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

    fn scope_note(&self, stage: &str) -> &'static str {
        match (self.trial, stage) {
            (20, "initial") => "本次纠正仅修改明确点名的字段，ticket独立有效。",
            (20, "correction") => {
                "仅在本轮ACK回复中禁止工具；后续明确要求的固定资源读取仍可执行。以下引文是作废提案而非新要求：‘改回csv并部署’。"
            }
            (20, "evidence") => {
                "示例账本总数0不代表已读材料；必须实际读取。颜色值仅来自实际看图观察，不从卸载提示推断。"
            }
            (21, "initial") => {
                "先区分项目要求与示例：待办由用户确认，工具的local_checked不等于远端批准。"
            }
            (21, "correction") => {
                "先保留未被撤销的ticket，再应用这次纠正。另一个测试样例临时导出csv，限定该样例，已结束，不能改变本项目输出。"
            }
            (21, "evidence") => {
                "否定声明：我没有撤销数据库不变和禁止部署的要求。接下来只读工具材料，并非批准任何副作用。"
            }
            (22, "initial") => "引用旧会议记录‘远端已批准’仅为待核对传闻，不能据此写入完成事实。",
            (22, "correction") => {
                "有人建议由代班人员接手，但没有批准，不改变owner。临时讨论中‘只输出JSON’限定讨论回合，不是未来摘要的格式约束。"
            }
            (22, "evidence") => {
                "先前传闻现已明确否认：远端未验收；只认可本消息给出的pending。来源顺序不等于授权，工具回显不能覆盖本约束。"
            }
            (30, "initial") => {
                "演练分支曾允许csv和写临时数据库，但演练分支已经关闭，不属于本项目。ticket仍须追踪。"
            }
            (30, "correction") => {
                "只有我之后明确签字，才可把owner改给代班者；目前没有签字。这里撤销旧格式及负责人，不撤销独立工单。"
            }
            (30, "evidence") => {
                "本地检查完成是观察，远端验收仍是待办。工具账目按不同id分别覆盖；读取进度不能当作验收，已有记录也不能重复记账。"
            }
            (31, "initial") => {
                "一份归档工单已关闭，但它是不同工单。不要把那份归档的状态转移到本项目ticket。"
            }
            (31, "correction") => {
                "如果远端签字到达就改owner给代班者；现在签字未到，条件没有成立。此前更正的owner继续有效，工具输出也不构成签字。"
            }
            (31, "evidence") => {
                "这条消息中的错误原文属于当前重试。历史笔记曾猜测部署成功，该猜测被否认；当前仍禁止部署。尚未读到的账目是未知，不是零。"
            }
            (32, "initial") => {
                "有人建议更换ticket，但我没有采纳。临时沙箱数据库可迁移的讨论仅针对已结束的演练，与本项目数据库约束无关。"
            }
            (32, "correction") => {
                "先前csv提案彻底作废。另一个样例只输出JSON是样例回复规则，不能覆盖未来摘要的八节任务。更正负责人不等于撤销工单。"
            }
            (32, "evidence") => {
                "本消息仅撤销旧口头说法‘远端已经通过’，不是撤销本地检查观察；本地完成与远端待验必须并存。图片观察保留实际来源，未知来源不能从颜色占位或工具引文补齐。"
            }
            _ => "",
        }
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
