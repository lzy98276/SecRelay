//! 字体目录：枚举系统字体、给出可用字重、把用户选择解析成渲染要用的 family + weight。
//!
//! # 为什么需要"解析"这一步
//!
//! 界面只知道用户选了「哪个字体、哪个字重」，但渲染需要的是
//! **family 名 + 数值字重**，而且这两者不一定直接对应：
//!
//! - 随应用分发的 miSans 只带两个字重文件，且它们的 family 名**不同**
//!   （`MiSans` 与 `MiSans Demibold`）—— 是独立字体族，不是同族权重。
//!   所以要靠切换 family 来表达字重，光给 `font-weight` 没用。
//! - 系统字体反过来：同族内有多个权重，靠 `font-weight` 挑；
//!   但**字体不一定有用户想要的权重**（很多中文字体只有 Regular + Bold），
//!   所以要挑一个实际存在的最近权重，否则渲染器会去合成假粗体。
//!
//! 把这件事集中在这里做，好处是可以用单元测试钉住，而不是靠肉眼看界面。

use std::collections::{BTreeMap, BTreeSet};

/// 随应用分发的 miSans 在界面上的 family 名（与字体内部一致）。
pub const BUILTIN_FAMILY: &str = "MiSans";

/// 内置 miSans 的粗体 family 名。
///
/// ⚠️ 这是 **miSans 自己的另一套 family**，不是 `MiSans` 的权重。
/// 上游静态字体每个字重一个 family，这一点有测试钉着。
pub const BUILTIN_BOLD_FAMILY: &str = "MiSans Demibold";

/// 内置 miSans 实际带的字重（Regular 400 / Demibold 600）。
pub const BUILTIN_WEIGHTS: [u16; 2] = [400, 600];

/// 默认字重。
pub const DEFAULT_WEIGHT: u16 = 400;

/// 标准字重档位，用于没有字重信息的兜底。
const STANDARD_WEIGHTS: [u16; 9] = [100, 200, 300, 400, 500, 600, 700, 800, 900];

/// 目录里的一个字体族。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontEntry {
    /// family 名（渲染时直接用这个名字）。
    pub family: String,
    /// 该族实际存在的字重，升序去重。
    pub weights: Vec<u16>,
    /// 是否是随应用分发的 miSans。
    pub builtin: bool,
}

/// 解析结果：渲染这个界面实际要用的 family 与字重。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFont {
    /// 常规文字用。
    pub family: String,
    /// 强调文字用（通常更粗）。
    pub bold_family: String,
    /// 常规文字字重。
    pub weight: u16,
    /// 强调文字字重。
    pub bold_weight: u16,
}

/// 系统字体目录。构造一次即可，查询是纯内存操作。
#[derive(Debug, Clone, Default)]
pub struct FontCatalog {
    entries: Vec<FontEntry>,
}

impl FontCatalog {
    /// 枚举系统字体（并加上内置 miSans）。
    ///
    /// 这个操作要读系统字体目录，在 Windows 上大约几百毫秒，
    /// 所以调用方应当只做一次并把结果留着。
    pub fn load() -> Self {
        let mut map: BTreeMap<String, BTreeSet<u16>> = BTreeMap::new();

        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        for face in db.faces() {
            let weight = face.weight.0;
            for (family, _language) in &face.families {
                let family = family.trim();
                if family.is_empty() {
                    continue;
                }
                map.entry(family.to_string()).or_default().insert(weight);
            }
        }

        let mut entries: Vec<FontEntry> = map
            .into_iter()
            .map(|(family, weights)| FontEntry {
                family,
                weights: weights.into_iter().collect(),
                builtin: false,
            })
            .collect();

        // 内置 miSans 放最前：它是默认项。
        // 如果系统里恰好也装了 MiSans，用内置的那份（结果一致，且保证一定有）。
        entries.retain(|entry| entry.family != BUILTIN_FAMILY);
        entries.insert(
            0,
            FontEntry {
                family: BUILTIN_FAMILY.to_string(),
                weights: BUILTIN_WEIGHTS.to_vec(),
                builtin: true,
            },
        );

        Self { entries }
    }

    /// 不读系统的空目录，只含内置 miSans。用于测试与加载失败时的兜底。
    pub fn builtin_only() -> Self {
        Self {
            entries: vec![FontEntry {
                family: BUILTIN_FAMILY.to_string(),
                weights: BUILTIN_WEIGHTS.to_vec(),
                builtin: true,
            }],
        }
    }

    /// 供下拉框展示的字体名列表（内置 miSans 在最前）。
    pub fn families(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.family.clone()).collect()
    }

    /// 某个字体族可用的字重；未知字体返回空。
    pub fn weights(&self, family: &str) -> Vec<u16> {
        self.entries
            .iter()
            .find(|e| e.family == family)
            .map(|e| e.weights.clone())
            .unwrap_or_default()
    }

    /// 下拉框里该列出的字重档位。
    ///
    /// 与 [`Self::weights`] 的区别：字体没有字重信息时（例如枚举失败或字体本身
    /// 不提供元数据）退回标准档位，而不是给一个空下拉框。
    pub fn selectable_weights(&self, family: &str) -> Vec<u16> {
        let weights = self.weights(family);
        if weights.is_empty() {
            STANDARD_WEIGHTS.to_vec()
        } else {
            weights
        }
    }

    /// 目录里有没有这个字体。
    pub fn contains(&self, family: &str) -> bool {
        self.entries.iter().any(|e| e.family == family)
    }

    /// 把「字体 + 期望字重」解析成渲染参数。
    ///
    /// 字体不存在时回退到内置 miSans —— 用户可能卸载了之前选的字体，
    /// 这时候界面必须还能正常显示，而不是变成一堆方框。
    pub fn resolve(&self, family: &str, weight: u16) -> ResolvedFont {
        let family = if self.contains(family) {
            family
        } else {
            BUILTIN_FAMILY
        };

        if family == BUILTIN_FAMILY {
            // 内置：靠切换 family 表达字重（两个文件是独立字体族）
            let bold = weight >= 600;
            return ResolvedFont {
                family: if bold { BUILTIN_BOLD_FAMILY } else { BUILTIN_FAMILY }.to_string(),
                bold_family: BUILTIN_BOLD_FAMILY.to_string(),
                weight: if bold { 600 } else { 400 },
                bold_weight: 600,
            };
        }

        // 系统字体：同族内挑实际存在的最近权重
        let available = self.weights(family);
        let available = if available.is_empty() {
            STANDARD_WEIGHTS.to_vec()
        } else {
            available
        };
        let normal = nearest_weight(&available, weight);
        // 强调文字至少要和常规有区分度；再挑一个实际存在的权重
        let bold_target = normal.max(700);
        let bold = nearest_weight(&available, bold_target);

        ResolvedFont {
            family: family.to_string(),
            bold_family: family.to_string(),
            weight: normal,
            bold_weight: bold,
        }
    }
}

/// 从候选里挑离 `target` 最近的权重；同样近时取较小的那个。
fn nearest_weight(available: &[u16], target: u16) -> u16 {
    available
        .iter()
        .copied()
        .min_by_key(|candidate| (candidate.abs_diff(target), *candidate))
        .unwrap_or(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 内置_misans_靠切换字体族表达字重() {
        let catalog = FontCatalog::builtin_only();

        let regular = catalog.resolve(BUILTIN_FAMILY, 400);
        assert_eq!(regular.family, "MiSans");
        assert_eq!(regular.weight, 400);
        // 强调文字换成另一个 family，而不是靠 font-weight
        assert_eq!(regular.bold_family, "MiSans Demibold");
        assert_eq!(regular.bold_weight, 600);

        let bold = catalog.resolve(BUILTIN_FAMILY, 600);
        assert_eq!(bold.family, "MiSans Demibold");
        assert_eq!(bold.weight, 600);
    }

    #[test]
    fn 内置字重只有两档() {
        assert_eq!(BUILTIN_WEIGHTS, [400, 600]);
        assert_eq!(FontCatalog::builtin_only().weights(BUILTIN_FAMILY), vec![400, 600]);
    }

    #[test]
    fn 内置排在第一位() {
        let catalog = FontCatalog::builtin_only();
        assert_eq!(catalog.families().first().map(String::as_str), Some("MiSans"));
    }

    #[test]
    fn 未知字体回退到内置() {
        let catalog = FontCatalog::builtin_only();
        let resolved = catalog.resolve("这个字体不存在", 400);
        assert_eq!(resolved.family, "MiSans");
        assert!(!catalog.contains("这个字体不存在"));
    }

    #[test]
    fn 系统字体挑实际存在的最近字重() {
        // 一个只有 400 / 700 的字体（很多中文字体就是这两档）
        let catalog = FontCatalog {
            entries: vec![
                FontEntry {
                    family: BUILTIN_FAMILY.to_string(),
                    weights: BUILTIN_WEIGHTS.to_vec(),
                    builtin: true,
                },
                FontEntry {
                    family: "只有两档的字体".to_string(),
                    weights: vec![400, 700],
                    builtin: false,
                },
            ],
        };

        // 想要 500 → 实际只能给 400 或 700，同样近时取较小
        let medium = catalog.resolve("只有两档的字体", 500);
        assert_eq!(medium.weight, 400, "500 到 400 和 700 一样近，应取较小");
        // 强调文字要真的更粗
        assert_eq!(medium.bold_weight, 700);

        // 想要 900 → 给 700
        assert_eq!(catalog.resolve("只有两档的字体", 900).weight, 700);
        // 想要 100 → 给 400
        assert_eq!(catalog.resolve("只有两档的字体", 100).weight, 400);
        // 系统字体的粗体是同族，靠 font-weight 生效
        let r = catalog.resolve("只有两档的字体", 400);
        assert_eq!(r.family, r.bold_family);
    }

    #[test]
    fn 最近字重取较小值() {
        assert_eq!(nearest_weight(&[400, 700], 550), 400);
        assert_eq!(nearest_weight(&[400, 700], 551), 700);
        assert_eq!(nearest_weight(&[400], 900), 400);
        assert_eq!(nearest_weight(&[], 500), 500);
    }

    #[test]
    fn 真实系统字体枚举不_panic_且包含内置项() {
        // 结果随机器变化，所以只断言"能跑通且内置项在"
        let catalog = FontCatalog::load();
        let families = catalog.families();
        assert!(!families.is_empty());
        assert_eq!(families[0], BUILTIN_FAMILY);

        let resolved = catalog.resolve(BUILTIN_FAMILY, DEFAULT_WEIGHT);
        assert_eq!(resolved.family, "MiSans");
    }
}
