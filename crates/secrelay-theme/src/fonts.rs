//! 字体目录：枚举系统字体、给出可用字重、把用户选择解析成渲染要用的 family + weight。
//!
//! # 为什么需要"解析"这一步
//!
//! 界面只知道用户选了「哪个字体、哪个字重」，但渲染需要的是
//! **family 名 + 数值字重**，而且这两者不一定直接对应：
//!
//! - 随应用分发的 miSans 每个字重一个文件，family 名取自 name 表 nameID 1：
//!   `MiSans Light` / `MiSans` / `MiSans Medium` / `MiSans Demibold` / `MiSans` ——
//!   Regular 与 Bold 共用 `MiSans`，靠 `font-weight` 区分，其余三个各自成族。
//!   所以字重主要靠切换 family 表达。
//! - 系统字体反过来：同族内有多个权重，靠 `font-weight` 挑；
//!   但**字体不一定有用户想要的权重**（很多中文字体只有 Regular + Bold），
//!   所以要挑一个实际存在的最近权重，否则渲染器会去合成假粗体。
//!
//! 把这件事集中在这里做，好处是可以用单元测试钉住，而不是靠肉眼看界面。

use std::collections::{BTreeMap, BTreeSet};

/// 随应用分发的 miSans 在界面上的 family 名（与字体内部一致）。
pub const BUILTIN_FAMILY: &str = "MiSans";

/// 内置 miSans 的字重档位，依次对应 Light / Regular / Medium / Demibold / Bold。
///
/// 档位是对外的通用值，用于界面展示；实际渲染用的字重见 [`builtin_render_weight`]。
pub const BUILTIN_WEIGHTS: [u16; 5] = [300, 400, 500, 600, 700];

/// 默认字重。
pub const DEFAULT_WEIGHT: u16 = 400;

/// 档位 → 字体文件里真实的 `usWeightClass`。
///
/// 这五个文件的实测值是非标准的 250/330/380/450/630，必须原样传给渲染器。
/// 传通用值是错的：传 400 时渲染器会在 250/330/380/450/630 里挑最近的 380
/// （Medium），而不是正文字重 330（Regular）。
pub fn builtin_render_weight(weight: u16) -> u16 {
    match weight {
        300 => 250,
        500 => 380,
        600 => 450,
        700 => 630,
        // 400 与未知值都当作 Regular
        _ => 330,
    }
}

/// 内置字体在渲染器里**只有一个 family**。
///
/// 这五个文件的 nameID 16（排版族名）都是 `MiSans`，而 fontique 注册字体时优先取它，
/// 所以 `"MiSans Light"` 这类名字在渲染器里并不存在，写上去会落到回退字体。
/// 区分字重只能靠 `font-weight`。
pub fn builtin_family(_weight: u16) -> &'static str {
    BUILTIN_FAMILY
}

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
    /// 渲染用的 family 名。
    pub family: String,
    /// 渲染用的字重（字体真实值，不是界面档位）。
    pub weight: u16,
    /// 界面档位，用于反查下拉框下标。
    pub tier: u16,
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

    /// 把请求的字重就近落到某个字体族实际提供的档位。
    ///
    /// 界面用它反查下拉框下标，避免"用户点了 500、实际渲染 400、下拉框还显示 500"。
    pub fn nearest_tier(&self, family: &str, weight: u16) -> u16 {
        let family = if self.contains(family) {
            family
        } else {
            BUILTIN_FAMILY
        };
        let available = if family == BUILTIN_FAMILY {
            BUILTIN_WEIGHTS.to_vec()
        } else {
            self.selectable_weights(family)
        };
        nearest_weight(&available, weight)
    }

    /// 把「字体 + 期望字重」解析成渲染参数。
    ///
    /// 字体不存在时回退到内置 miSans —— 用户可能卸载了之前选的字体，
    /// 这时候界面必须还能正常显示，而不是变成一堆方框。
    pub fn resolve(&self, family: &str, weight: u16) -> ResolvedFont {
        let tier = self.nearest_tier(family, weight);
        let family = if self.contains(family) {
            family
        } else {
            BUILTIN_FAMILY
        };

        if family == BUILTIN_FAMILY {
            // 内置：family 恒为 "MiSans"，字重取档位对应的真实 usWeightClass
            return ResolvedFont {
                family: BUILTIN_FAMILY.to_string(),
                weight: builtin_render_weight(tier),
                tier,
            };
        }

        // 系统字体：同族内挑实际存在的最近权重，档位就是该权重本身
        ResolvedFont {
            family: family.to_string(),
            weight: tier,
            tier,
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
    fn 内置字体只有一个_family() {
        let catalog = FontCatalog::builtin_only();

        // 五个文件在渲染器里同属 "MiSans"，family 名区分不了字重
        for tier in BUILTIN_WEIGHTS {
            assert_eq!(catalog.resolve(BUILTIN_FAMILY, tier).family, "MiSans", "档位 {tier}");
        }
    }

    #[test]
    fn 内置档位映射到真实字重() {
        let catalog = FontCatalog::builtin_only();

        // 渲染器只认 font-weight，所以必须传字体文件里的真实 usWeightClass
        for (tier, render) in [(300, 250), (400, 330), (500, 380), (600, 450), (700, 630)] {
            let resolved = catalog.resolve(BUILTIN_FAMILY, tier);
            assert_eq!(resolved.weight, render, "档位 {tier}");
            assert_eq!(resolved.tier, tier);
        }
        assert_eq!(builtin_render_weight(DEFAULT_WEIGHT), 330, "默认档位要落到 Regular");
    }

    #[test]
    fn 内置字重是五档() {
        assert_eq!(BUILTIN_WEIGHTS, [300, 400, 500, 600, 700]);
        assert_eq!(
            FontCatalog::builtin_only().weights(BUILTIN_FAMILY),
            vec![300, 400, 500, 600, 700]
        );
    }

    #[test]
    fn 内置字重就近落档() {
        let catalog = FontCatalog::builtin_only();

        // 450 到 400 和 500 一样近，取较小
        assert_eq!(catalog.resolve(BUILTIN_FAMILY, 450).tier, 400);
        assert_eq!(catalog.resolve(BUILTIN_FAMILY, 500).tier, 500);
        // 651 离 700（差 49）比离 600（差 51）近
        assert_eq!(catalog.resolve(BUILTIN_FAMILY, 651).tier, 700);
        assert_eq!(catalog.resolve(BUILTIN_FAMILY, 100).tier, 300);
        assert_eq!(catalog.resolve(BUILTIN_FAMILY, 900).tier, 700);

        // 档位必须是内置档位之一，否则界面反查下拉框下标会落空
        for requested in [100, 300, 350, 400, 450, 500, 650, 900] {
            let tier = catalog.resolve(BUILTIN_FAMILY, requested).tier;
            assert!(BUILTIN_WEIGHTS.contains(&tier), "{requested} 落到了 {tier}");
        }
    }

    #[test]
    fn 五个档位映射到互不相同的真实字重() {
        // 否则渲染器会把两个档位解析成同一个字体面
        let mut weights: Vec<u16> = BUILTIN_WEIGHTS
            .iter()
            .map(|tier| builtin_render_weight(*tier))
            .collect();
        let before = weights.len();
        weights.sort_unstable();
        weights.dedup();
        assert_eq!(weights.len(), before, "真实字重有重复");
    }

    #[test]
    fn 档位越大真实字重越大() {
        for pair in BUILTIN_WEIGHTS.windows(2) {
            assert!(
                builtin_render_weight(pair[0]) < builtin_render_weight(pair[1]),
                "档位 {pair:?} 的真实字重没有递增"
            );
        }
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
        assert_eq!(resolved.tier, 400);
        assert_eq!(resolved.weight, 330, "回退后应落到 miSans 的 Regular");
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

        // 想要 900 → 给 700
        assert_eq!(catalog.resolve("只有两档的字体", 900).weight, 700);
        // 想要 100 → 给 400
        assert_eq!(catalog.resolve("只有两档的字体", 100).weight, 400);
        // 系统字体的 family 名原样透传
        assert_eq!(catalog.resolve("只有两档的字体", 400).family, "只有两档的字体");
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
        assert!(BUILTIN_WEIGHTS.contains(&resolved.tier));
        assert_eq!(resolved.family, "MiSans");
    }
}
