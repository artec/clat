use super::*;

/// 二级选择器动作，由 App 决定立即保存还是转交编辑器补密钥。
#[derive(Debug)]
pub(crate) enum PickerAction {
    Continue,
    /// 用户在二级列表确认了某个预设。
    SelectPreset(&'static ModelPreset),
    /// dsh 形态（D-2 §2.5）：确认了宿主组的某个模型——
    /// `selectModel { provider: group.id, model: model.id }`；effort 是
    /// 高亮行上 Shift+Tab 循环出的待提交档位（档位接入 2026-08-23；
    /// None = 不带，宿主 adapter 默认）。
    SelectDshModel {
        provider: String,
        model: String,
        effort: Option<String>,
    },
    /// B9：Custom 入口三态派发（零档案 → 新建模板；列表内 `New…` →
    /// 新建模板；`e` → 编辑既有档案）。
    OpenProfileEditor {
        /// None = 空白新建模板；Some = 编辑该档案。
        edit: Option<String>,
    },
    /// B9：Enter 确认切换到该档案（切换并关闭）。
    SwitchProfile(String),
    /// B9：两步确认后删除该档案（actions 侧走回退门面）。
    DeleteProfile(String),
    Cancel,
}

/// dsh 模型目录的 UI 适配（宿主 `session.models` 应答 → 两级 picker
/// 数据；D-2 §2.5：内置与自定义组一视同仁，数据全宿主动态，不硬编码
/// 任何厂商）。
pub(crate) struct DshModelData {
    pub(crate) groups: Vec<DshModelGroup>,
    /// 失败组（一级尾部灰行，诚实呈现不静默丢组）。
    pub(crate) failures: Vec<DshModelFailure>,
    /// 当前所选 (provider, model id)——当前行行首前置列标 `✓`。
    pub(crate) current: Option<(String, String)>,
    /// 当前选择的档位 id（`current.reasoningEffort`；缺席 = 不带档位）。
    pub(crate) current_effort: Option<String>,
}

pub(crate) struct DshModelGroup {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) models: Vec<DshModelEntry>,
}

pub(crate) struct DshModelEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// 该模型的可选档位（`reasoning.efforts`，宿主 adapter 自有词汇、
    /// 宿主展示序；空 = 无推理档位的模型——Shift+Tab 不可达）。
    pub(crate) efforts: Vec<DshEffort>,
}

/// 一个可选档位（id + 宿主展示名，如 `high` / `High`）。
pub(crate) struct DshEffort {
    pub(crate) id: String,
    pub(crate) name: String,
}

pub(crate) struct DshModelFailure {
    pub(crate) name: String,
    pub(crate) message: String,
}

/// 宿主 models 应答（groups/failures/current）→ picker 数据；groups
/// 与 failures 皆空 → None（上层 flash "no models available"）。
pub(crate) fn dsh_model_data_from(value: &serde_json::Value) -> Option<DshModelData> {
    let group = |entry: &serde_json::Value| DshModelGroup {
        id: entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_owned(),
        name: entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
            .to_owned(),
        models: entry
            .get("models")
            .and_then(serde_json::Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .map(|model| DshModelEntry {
                        id: model
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("?")
                            .to_owned(),
                        name: model
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("?")
                            .to_owned(),
                        description: model
                            .get("description")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                        efforts: model
                            .get("reasoning")
                            .and_then(|reasoning| reasoning.get("efforts"))
                            .and_then(serde_json::Value::as_array)
                            .map(|efforts| {
                                efforts
                                    .iter()
                                    .filter_map(|effort| {
                                        Some(DshEffort {
                                            id: effort
                                                .get("id")
                                                .and_then(serde_json::Value::as_str)?
                                                .to_owned(),
                                            name: effort
                                                .get("name")
                                                .and_then(serde_json::Value::as_str)
                                                .unwrap_or("?")
                                                .to_owned(),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    let groups: Vec<DshModelGroup> = value
        .get("groups")
        .and_then(serde_json::Value::as_array)
        .map(|entries| entries.iter().map(group).collect())
        .unwrap_or_default();
    let failures: Vec<DshModelFailure> = value
        .get("failures")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| DshModelFailure {
                    name: entry
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?")
                        .to_owned(),
                    message: entry
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    if groups.is_empty() && failures.is_empty() {
        return None;
    }
    let current = value.get("current").and_then(|current| {
        let provider = current
            .get("provider")
            .and_then(serde_json::Value::as_str)?;
        let model = current.get("model").and_then(serde_json::Value::as_str)?;
        Some((provider.to_owned(), model.to_owned()))
    });
    let current_effort = value
        .get("current")
        .and_then(|current| current.get("reasoningEffort"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some(DshModelData {
        groups,
        failures,
        current,
        current_effort,
    })
}

/// B9：档案列表条目（picker 注入的只读摘要）。
pub(crate) struct ProfileSummary {
    pub name: String,
    pub endpoint: String,
    pub model: String,
    pub active: bool,
    /// VP-3：只由档案 config 的 `capabilities.accepts_image_input()` 派生。
    pub image_input: bool,
}

/// VP-3 返工二轮（2026-09-03 负责人裁定）：选择器名称列的舒适定宽
/// ——≈ 最长内置名（"DeepSeek V4.0 Flash Vision (Exp)"，32 列）+
/// ` ⧉` + 余量；内置名永不截断，超宽档案名整体省略号截断，hint 从
/// 该固定列起排——间距舒适、位置恒定（右对齐右缘案与标签右邻案
/// 均已被否，见 open-worklist VP-3 返工记录）。
const MODEL_NAME_COLUMN: usize = 40;

/// Claude Code 风格的二级 /model 选择器：
///
/// - 一级：厂商列表（内置预设按 vendor 去重 + Custom 入口）
/// - 二级：该厂商的模型列表
///
/// Enter 进入/确认，Esc 在二级返回一级、在一级关闭，数字键 1-9 快选，
/// 鼠标点击行等价于选中并 Enter。
pub(crate) struct ModelPicker {
    /// 当前展示的厂商；None 表示一级列表。
    vendor: Option<&'static str>,
    selected: usize,
    /// INV-U1（原位返回）：进入下级（厂商二级/Custom 档案列表）时的
    /// 一级行号——Esc 返回时光标原位恢复，不重置到首行。
    home_row: usize,
    /// 当前配置来自的预设，用于在列表中标记 current。
    current_preset: Option<&'static ModelPreset>,
    /// B9：自定义档案（来自控制面注册表）。
    profiles: Vec<ProfileSummary>,
    /// B9：是否正在展示 Custom 档案列表。
    custom_list: bool,
    /// B9：删除两步确认——首按 `d` 记住待删行；再按 `d` 确认，其余
    /// 任意键取消（INV-M3：删除须显式确认）。
    confirm_delete: Option<usize>,
    /// dsh 数据形态（D-2 §2.5）：Some 时两级全宿主动态（一级 = groups，
    /// 二级 = 组内模型行），local 字段全部不读；`e`/`d`/`New…`/Custom
    /// 三态在 dsh 态不可达（无 Custom 行、custom_list 恒 false）。
    dsh: Option<DshModelData>,
    /// dsh 二级所在组下标（None = dsh 一级）。
    dsh_group: Option<usize>,
    /// dsh 二级高亮行上 Shift+Tab 循环出的待提交档位 id（档位接入
    /// 2026-08-23）；导航/换行/退级即清——只属于它被循环的那一行。
    dsh_effort: Option<String>,
}

/// B9 修复（INV-U1 原位返回，用户反馈 2026-08-22）：进入编辑器前对
/// picker 导航态拍照；编辑器取消后按快照原位重建（层级 + 光标行），
/// 选择链路不再整体消失。
#[derive(Clone, Debug)]
pub(crate) struct PickerSnapshot {
    vendor: Option<&'static str>,
    custom_list: bool,
    selected: usize,
    home_row: usize,
}

impl ModelPicker {
    pub fn new(config: &ModelConfig, profiles: Vec<ProfileSummary>) -> Self {
        let current_preset = config.preset.as_deref().and_then(preset_by_id);
        let active = profiles.iter().find(|profile| profile.active);
        let _ = active;
        Self {
            vendor: None,
            selected: 0,
            home_row: 0,
            current_preset,
            profiles,
            custom_list: false,
            confirm_delete: None,
            dsh: None,
            dsh_group: None,
            dsh_effort: None,
        }
    }

    /// dsh 形态构造（两级骨架复用，数据全宿主动态）。
    pub fn new_dsh(data: DshModelData) -> Self {
        Self {
            vendor: None,
            selected: 0,
            home_row: 0,
            current_preset: None,
            profiles: Vec::new(),
            custom_list: false,
            confirm_delete: None,
            dsh: Some(data),
            dsh_group: None,
            dsh_effort: None,
        }
    }

    pub fn row_count(&self) -> usize {
        self.rows().len()
    }

    /// 测试探针：当前光标行（INV-U1 原位返回断言用）。
    #[cfg(test)]
    pub(crate) fn selected_index(&self) -> usize {
        self.selected
    }

    /// INV-U1：导航态快照（进入编辑器前拍）。
    pub(crate) fn snapshot(&self) -> PickerSnapshot {
        PickerSnapshot {
            vendor: self.vendor,
            custom_list: self.custom_list,
            selected: self.selected,
            home_row: self.home_row,
        }
    }

    /// INV-U1：按快照原位恢复（行数变化时钳制到末行）。
    pub(crate) fn restore_snapshot(&mut self, snapshot: PickerSnapshot) {
        self.vendor = snapshot.vendor;
        self.custom_list = snapshot.custom_list;
        self.home_row = snapshot.home_row;
        self.selected = snapshot.selected.min(self.row_count().saturating_sub(1));
        self.confirm_delete = None;
    }

    fn rows(&self) -> Vec<PickerRow> {
        if self.custom_list {
            // B9 档案列表：档案行 + 底行 New…。
            let mut rows: Vec<PickerRow> = self
                .profiles
                .iter()
                .map(|profile| PickerRow::Profile(profile.name.clone()))
                .collect();
            rows.push(PickerRow::NewProfile);
            return rows;
        }
        // dsh 形态：一级 = 宿主组行 + 失败组灰行；二级 = 组内模型行。
        if let Some(dsh) = &self.dsh {
            return match self.dsh_group {
                None => {
                    let mut rows: Vec<PickerRow> =
                        (0..dsh.groups.len()).map(PickerRow::DshGroup).collect();
                    rows.extend((0..dsh.failures.len()).map(PickerRow::DshFailure));
                    rows
                }
                Some(group_index) => dsh
                    .groups
                    .get(group_index)
                    .map(|group| {
                        (0..group.models.len())
                            .map(|model| PickerRow::DshModel(group_index, model))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            };
        }
        match self.vendor {
            None => {
                let mut rows: Vec<PickerRow> = preset_vendors()
                    .into_iter()
                    .map(PickerRow::Vendor)
                    .collect();
                rows.push(PickerRow::Custom);
                rows
            }
            Some(vendor) => presets_by_vendor(vendor)
                .into_iter()
                .map(PickerRow::Preset)
                .collect(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        if self.custom_list {
            return self.handle_custom_list_key(key);
        }
        // 二级返回一级（INV-U1：光标回到进入时的行）——local 与 dsh 同款。
        let in_second_level = self.vendor.is_some() || self.dsh_group.is_some();
        match key.code {
            KeyCode::Esc | KeyCode::Left if in_second_level => {
                self.vendor = None;
                self.dsh_group = None;
                self.dsh_effort = None;
                self.selected = self.home_row.min(self.row_count().saturating_sub(1));
                PickerAction::Continue
            }
            KeyCode::Esc => PickerAction::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = (self.selected + self.row_count() - 1) % self.row_count();
                // 档位 pending 只属于它被循环的那一行（换行即弃）。
                self.dsh_effort = None;
                PickerAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % self.row_count();
                self.dsh_effort = None;
                PickerAction::Continue
            }
            // dsh 二级高亮模型行：循环待提交档位（宿主 adapter 自有
            // 词汇表；无档位模型为无操作）。
            KeyCode::BackTab if self.dsh.is_some() && self.dsh_group.is_some() => {
                self.cycle_dsh_effort();
                PickerAction::Continue
            }
            KeyCode::Enter | KeyCode::Right => self.activate(self.selected),
            KeyCode::Char(ch) if ch.is_ascii_digit() && ch != '0' => {
                let index = (ch as usize - '1' as usize).min(8);
                if index < self.row_count() {
                    self.activate(index)
                } else {
                    PickerAction::Continue
                }
            }
            _ => PickerAction::Continue,
        }
    }

    /// dsh 二级高亮模型行的档位循环（Shift+Tab）：pending 已属于本
    /// 模型 → 前进一档（回绕）；否则从当前档位的下一档起步（当前
    /// 模型无当前档 / 非当前模型 → 首档，宿主展示序）。无档位模型
    /// 无操作。当前档位来自目录 `current.reasoningEffort`。
    fn cycle_dsh_effort(&mut self) {
        let Some(data) = self.dsh.as_ref() else {
            return;
        };
        let rows = self.rows();
        let Some(PickerRow::DshModel(group_index, model_index)) = rows.get(self.selected) else {
            return;
        };
        let Some(model) = data
            .groups
            .get(*group_index)
            .and_then(|group| group.models.get(*model_index))
        else {
            return;
        };
        if model.efforts.is_empty() {
            return;
        }
        // 高亮行是否当前所选模型：是 → 目录的当前档位作循环起点。
        let is_current = data.current.as_ref().is_some_and(|(provider, id)| {
            data.groups.get(*group_index).is_some_and(|group| {
                provider == &group.id && group.models.get(*model_index).is_some_and(|m| id == &m.id)
            })
        });
        let current_effort = if is_current {
            data.current_effort.clone()
        } else {
            None
        };
        let pending = self.dsh_effort.take();
        let next = match pending.as_deref() {
            Some(p) if model.efforts.iter().any(|e| e.id == p) => {
                let index = model
                    .efforts
                    .iter()
                    .position(|e| e.id == p)
                    .expect("checked");
                model.efforts[(index + 1) % model.efforts.len()].id.clone()
            }
            _ => match current_effort
                .as_deref()
                .and_then(|c| model.efforts.iter().position(|e| e.id == c))
            {
                Some(index) => model.efforts[(index + 1) % model.efforts.len()].id.clone(),
                None => model.efforts[0].id.clone(),
            },
        };
        self.dsh_effort = Some(next);
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> PickerAction {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return PickerAction::Continue;
        }
        if mouse.column < area.x || mouse.column >= area.x + area.width {
            return PickerAction::Continue;
        }
        if mouse.row <= area.y || mouse.row >= area.y + area.height.saturating_sub(1) {
            return PickerAction::Continue;
        }
        let row = mouse.row.saturating_sub(area.y + 1) as usize;
        // 可见内容行数 = 弹框高 - 双边框 - 空行 - 钉底说明行；空行/
        // 说明行上的点击以及被裁剪的行不可激活。
        let visible_rows = area.height.saturating_sub(4) as usize;
        if row >= self.row_count() || row >= visible_rows {
            return PickerAction::Continue;
        }
        self.activate(row)
    }

    fn activate(&mut self, index: usize) -> PickerAction {
        if self.custom_list {
            return match self.rows().get(index) {
                Some(PickerRow::Profile(name)) => PickerAction::SwitchProfile(name.clone()),
                Some(PickerRow::NewProfile) => PickerAction::OpenProfileEditor { edit: None },
                _ => PickerAction::Continue,
            };
        }
        // dsh 形态：组行进二级；模型行确认；失败组灰行不可选。
        if let Some(dsh) = &self.dsh {
            return match self.rows().get(index) {
                Some(PickerRow::DshGroup(group)) => {
                    self.home_row = index;
                    self.dsh_group = Some(*group);
                    self.dsh_effort = None;
                    self.selected = 0;
                    PickerAction::Continue
                }
                Some(PickerRow::DshModel(group, model)) => {
                    match (
                        dsh.groups.get(*group),
                        dsh.groups.get(*group).and_then(|g| g.models.get(*model)),
                    ) {
                        (Some(group), Some(model)) => PickerAction::SelectDshModel {
                            provider: group.id.clone(),
                            model: model.id.clone(),
                            // 档位只随高亮行提交（数字快选他行不带
                            // pending；换模型不带旧档位）。
                            effort: if index == self.selected {
                                self.dsh_effort
                                    .clone()
                                    .filter(|effort| model.efforts.iter().any(|e| &e.id == effort))
                            } else {
                                None
                            },
                        },
                        _ => PickerAction::Continue,
                    }
                }
                _ => PickerAction::Continue,
            };
        }
        match self.rows().get(index) {
            Some(PickerRow::Vendor(vendor)) => {
                self.home_row = index;
                self.vendor = Some(vendor);
                self.selected = 0;
                PickerAction::Continue
            }
            Some(PickerRow::Preset(preset)) => PickerAction::SelectPreset(preset),
            Some(PickerRow::Custom) => {
                // B9 三态：零档案 → 直接进新建页；≥1 → 档案列表。
                if self.profiles.is_empty() {
                    PickerAction::OpenProfileEditor { edit: None }
                } else {
                    self.home_row = index;
                    self.custom_list = true;
                    self.selected = 0;
                    self.confirm_delete = None;
                    PickerAction::Continue
                }
            }
            _ => PickerAction::Continue,
        }
    }

    /// B9：档案列表键位——Enter 切换并关闭、`e` 编辑、`d` 两步确认
    /// 删除、New… 行 Enter 新建、Esc 回一级。任意非 `d` 键取消确认态。
    fn handle_custom_list_key(&mut self, key: KeyEvent) -> PickerAction {
        if let Some(pending) = self.confirm_delete {
            if key.code == KeyCode::Char('d') {
                self.confirm_delete = None;
                let name = match self.rows().get(pending) {
                    Some(PickerRow::Profile(name)) => name.clone(),
                    _ => return PickerAction::Continue,
                };
                return PickerAction::DeleteProfile(name);
            }
            self.confirm_delete = None;
            return PickerAction::Continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Left => {
                // 回一级（INV-U1：光标回到进入时的 Custom 行）。
                self.custom_list = false;
                self.selected = self.home_row.min(self.row_count().saturating_sub(1));
                PickerAction::Continue
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = (self.selected + self.row_count() - 1) % self.row_count();
                PickerAction::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % self.row_count();
                PickerAction::Continue
            }
            KeyCode::Enter | KeyCode::Right => self.activate(self.selected),
            KeyCode::Char('e') => match self.rows().get(self.selected) {
                Some(PickerRow::Profile(name)) => PickerAction::OpenProfileEditor {
                    edit: Some(name.clone()),
                },
                _ => PickerAction::Continue,
            },
            KeyCode::Char('d') => match self.rows().get(self.selected) {
                Some(PickerRow::Profile(_)) => {
                    self.confirm_delete = Some(self.selected);
                    PickerAction::Continue
                }
                _ => PickerAction::Continue,
            },
            KeyCode::Char(ch) if ch.is_ascii_digit() && ch != '0' => {
                let index = (ch as usize - '1' as usize).min(8);
                if index < self.row_count() {
                    self.activate(index)
                } else {
                    PickerAction::Continue
                }
            }
            _ => PickerAction::Continue,
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        crate::tui::clear_popup_with_guards(frame, area);
        // 弹窗规范统一（2026-08-22 用户反馈）：键位说明钉在弹框底部、
        // Faint 灰、与内容恰好隔一空行——与 /resume /perm /help
        // /mcp 一致；此前说明行随 Paragraph 内容浮动，小列表（max(8)
        // 兜底撑高）悬空、各级观感不一。内容行超高时被裁剪，说明行
        // 永不被挤出框外。VP-3 返工二轮（2026-09-03）：能力图例并入
        // 说明行行尾（`· ⧉ images`），不再单占一行——一处足矣，
        // 拒绝重复张贴。
        let title = if let Some(dsh) = &self.dsh {
            match self.dsh_group.and_then(|index| dsh.groups.get(index)) {
                Some(group) => format!("/model · {}", group.name),
                None => "/model".to_owned(),
            }
        } else if self.custom_list {
            "/model · Custom".to_owned()
        } else {
            match self.vendor {
                None => "/model".to_owned(),
                Some(vendor) => format!("/model · {vendor}"),
            }
        };
        let block = crate::tui::popup_block(&title);
        let inner = block.inner(area);
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
        frame.render_widget(block, area);

        let mut lines = Vec::new();
        let row_width = content_area.width as usize;
        for (index, row) in self.rows().iter().enumerate() {
            let (mut label, mut hint, current, image_input) = self.row_display(index);
            if self.confirm_delete == Some(index) {
                label = format!("delete {label}?");
                hint = "d again to confirm · any other key cancels".into();
            }
            let style = if index == self.selected {
                crate::tui::theme::style(crate::tui::theme::Role::Selected)
            } else if matches!(row, PickerRow::DshFailure(_)) {
                // 失败组灰行：诚实呈现宿主的失败组（不可选，Enter 无动作）。
                crate::tui::theme::style(crate::tui::theme::Role::Faint)
            } else {
                Style::default()
            };
            // VP-3 四轮定稿（2026-09-03 负责人裁定）：✓ 锚定名称——
            // 居数字列之后、名称列之前（`1 ✓ 名称⧉`）；名称 + ⧉ 渲染
            // 为一个单元，超宽省略号截断（列宽 ≈ 最长内置名 + ⧉ + 余
            // 量，内置名永不截断），hint 起排位置恒定，不随名称长度
            // 漂移（根治 hint 尾挂与撞字）。
            if image_input {
                label.push_str(" ⧉");
            }
            let name_column = crate::tui::fit_display_width(&label, MODEL_NAME_COLUMN);
            let number = if index < 9 {
                format!("{}", index + 1)
            } else {
                " ".into()
            };
            // 列表行保持单行：超宽整体截尾加省略号——行数即内容高度，
            // 说明行与内容恒隔一空行。
            lines.push(Line::from(Span::styled(
                crate::tui::numbered_picker_row(
                    &number,
                    &format!("{name_column}{hint}"),
                    current,
                    row_width,
                ),
                style,
            )));
        }
        lines.push(Line::from(""));
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            content_area,
        );

        let footer = if self.custom_list {
            "↑↓ select · Enter switch · e edit · d delete · Esc back"
        } else if self.dsh.is_some() {
            match self.dsh_group {
                None => "↑↓ select · Enter open · 1-9 quick pick · Esc close",
                // 高亮模型有档位表才提示 ⇧Tab（无档位模型不可达）。
                Some(_) => {
                    let has_efforts = match self.rows().get(self.selected) {
                        Some(PickerRow::DshModel(group, model)) => self
                            .dsh
                            .as_ref()
                            .and_then(|dsh| dsh.groups.get(*group))
                            .and_then(|group| group.models.get(*model))
                            .is_some_and(|model| !model.efforts.is_empty()),
                        _ => false,
                    };
                    if has_efforts {
                        "↑↓ select · ⇧Tab effort · Enter confirm · Esc back"
                    } else {
                        "↑↓ select · Enter confirm · Esc back"
                    }
                }
            }
        } else {
            match self.vendor {
                None => "↑↓ select · Enter open · 1-9 quick pick · Esc close",
                Some(_) => "↑↓ select · Enter confirm · Esc back",
            }
        };
        // VP-3：能力图例只留此处（local 形态说明行行尾；dsh 不猜能力
        // 不显示）。
        let mut footer = footer.to_owned();
        if self.dsh.is_none() {
            footer.push_str(" · ⧉ images");
        }
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                footer,
                crate::tui::theme::style(crate::tui::theme::Role::Faint),
            ))]),
            footer_area,
        );
    }

    fn row_display(&self, index: usize) -> (String, String, bool, bool) {
        let rows = self.rows();
        let Some(row) = rows.get(index) else {
            return (String::new(), String::new(), false, false);
        };
        if let Some(dsh) = &self.dsh {
            return match row {
                PickerRow::DshGroup(index) => match dsh.groups.get(*index) {
                    Some(group) => {
                        let current = dsh
                            .current
                            .as_ref()
                            .is_some_and(|(provider, _)| *provider == group.id);
                        (
                            group.name.clone(),
                            format!("{} models", group.models.len()),
                            current,
                            false,
                        )
                    }
                    None => (String::new(), String::new(), false, false),
                },
                PickerRow::DshModel(group_index, model_index) => {
                    match dsh
                        .groups
                        .get(*group_index)
                        .and_then(|group| group.models.get(*model_index))
                    {
                        Some(model) => {
                            let current = dsh.current.as_ref().is_some_and(|(provider, id)| {
                                dsh.groups
                                    .get(*group_index)
                                    .is_some_and(|group| *provider == group.id)
                                    && id == &model.id
                            });
                            // 档位呈现：高亮行的 Shift+Tab pending 优先；
                            // 否则当前模型行常显其当前档位（efforts 表
                            // 解析展示名，未命中回落裸 id）。
                            let pending = if index == self.selected {
                                self.dsh_effort
                                    .clone()
                                    .filter(|effort| model.efforts.iter().any(|e| &e.id == effort))
                            } else {
                                None
                            };
                            let effort = pending.or_else(|| {
                                let id =
                                    current.then_some(dsh.current_effort.as_deref()).flatten()?;
                                let name = model
                                    .efforts
                                    .iter()
                                    .find(|e| e.id == id)
                                    .map(|e| e.name.clone())
                                    .unwrap_or_else(|| id.to_owned());
                                Some(name)
                            });
                            let hint = match effort {
                                Some(effort) => {
                                    format!(
                                        "{} · {}",
                                        model
                                            .description
                                            .clone()
                                            .unwrap_or_else(|| model.id.clone()),
                                        effort
                                    )
                                }
                                None => model
                                    .description
                                    .clone()
                                    .unwrap_or_else(|| model.id.clone()),
                            };
                            (model.name.clone(), hint, current, false)
                        }
                        None => (String::new(), String::new(), false, false),
                    }
                }
                PickerRow::DshFailure(index) => match dsh.failures.get(*index) {
                    Some(failure) => (
                        format!("{} ⚠", failure.name),
                        failure.message.clone(),
                        false,
                        false,
                    ),
                    None => (String::new(), String::new(), false, false),
                },
                _ => (String::new(), String::new(), false, false),
            };
        }
        match row {
            PickerRow::Vendor(vendor) => {
                let count = presets_by_vendor(vendor).len();
                let current = self
                    .current_preset
                    .is_some_and(|preset| preset.vendor == *vendor);
                (
                    (*vendor).to_owned(),
                    format!("{count} models"),
                    current,
                    false,
                )
            }
            PickerRow::Preset(preset) => (
                preset.name.to_owned(),
                preset.description.to_owned(),
                self.current_preset
                    .is_some_and(|current| current.id == preset.id),
                preset.owned_capabilities().accepts_image_input(),
            ),
            PickerRow::Custom => {
                let count = self.profiles.len();
                let hint = if count == 0 {
                    "create your first custom model".to_owned()
                } else {
                    format!("{count} custom model{}", if count == 1 { "" } else { "s" })
                };
                (
                    "Custom".to_owned(),
                    hint,
                    self.current_preset.is_none(),
                    false,
                )
            }
            PickerRow::Profile(name) => {
                let profile = self
                    .profiles
                    .iter()
                    .find(|profile| &profile.name == name)
                    .expect("picker rows mirror the profile list");
                (
                    name.clone(),
                    format!("{} · {}", profile.endpoint, profile.model),
                    profile.active,
                    profile.image_input,
                )
            }
            PickerRow::NewProfile => ("New…".to_owned(), "blank template".to_owned(), false, false),
            // dsh 行在上方 dsh 分支早退，此处不可达。
            PickerRow::DshGroup(_) | PickerRow::DshModel(_, _) | PickerRow::DshFailure(_) => {
                (String::new(), String::new(), false, false)
            }
        }
    }
}

enum PickerRow {
    Vendor(&'static str),
    Preset(&'static ModelPreset),
    Custom,
    /// B9：档案列表行（携带档案名）。
    Profile(String),
    /// B9：档案列表底行——新建。
    NewProfile,
    /// dsh 一级：宿主 provider 组（携带组下标）。
    DshGroup(usize),
    /// dsh 二级：组内模型行（组下标, 模型下标）。
    DshModel(usize, usize),
    /// dsh 一级尾部：失败组灰行（不可选）。
    DshFailure(usize),
}
