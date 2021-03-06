use crate::{
    assets::{self, AssetExt},
    lottery::LootSpec,
    recipe::{default_recipe_book, RecipeInput},
    trade::Good,
};
use assets::AssetGuard;
use hashbrown::HashMap;
use lazy_static::lazy_static;
use serde::Deserialize;
use std::cmp::Ordering;
use tracing::{info, warn};

const PRICING_DEBUG: bool = false;

#[derive(Default, Debug)]
pub struct TradePricing {
    // items of different good kinds
    tools: Entries,
    armor: Entries,
    potions: Entries,
    food: Entries,
    ingredients: Entries,
    other: Entries,

    // good_scaling of coins
    coin_scale: f32,

    // get amount of material per item
    material_cache: HashMap<String, (Good, f32)>,
    equality_set: EqualitySet,
}

// item asset specifier, probability, whether it's sellable by merchants
type Entry = (String, f32, bool);

#[derive(Default, Debug)]
struct Entries {
    entries: Vec<Entry>,
}

impl Entries {
    fn add(&mut self, eqset: &EqualitySet, item_name: &str, probability: f32, can_sell: bool) {
        let canonical_itemname = eqset.canonical(item_name);

        let old = self
            .entries
            .iter_mut()
            .find(|(name, _, _)| *name == *canonical_itemname);

        // Increase probability if already in entries, or add new entry
        if let Some((asset, ref mut old_probability, _)) = old {
            if PRICING_DEBUG {
                info!("Update {} {}+{}", asset, old_probability, probability);
            }
            *old_probability += probability;
        } else {
            if PRICING_DEBUG {
                info!("New {} {}", item_name, probability);
            }
            self.entries
                .push((canonical_itemname.to_owned(), probability, can_sell));
        }

        // Add the non-canonical item so that it'll show up in merchant inventories
        // It will have infinity as its price, but it's fine,
        // because we determine all prices based on canonical value
        if canonical_itemname != item_name
            && !self.entries.iter().any(|(name, _, _)| name == item_name)
        {
            self.entries.push((item_name.to_owned(), 0.0, can_sell));
        }
    }
}

lazy_static! {
    static ref TRADE_PRICING: TradePricing = TradePricing::read();
}

#[derive(Clone)]
/// A collection of items with probabilty (normalized to one), created
/// hierarchically from `LootSpec`s
/// (probability, item id, average amount)
pub struct ProbabilityFile {
    pub content: Vec<(f32, String, f32)>,
}

impl assets::Asset for ProbabilityFile {
    type Loader = assets::LoadFrom<Vec<(f32, LootSpec<String>)>, assets::RonLoader>;

    const EXTENSION: &'static str = "ron";
}

impl From<Vec<(f32, LootSpec<String>)>> for ProbabilityFile {
    #[allow(clippy::cast_precision_loss)]
    fn from(content: Vec<(f32, LootSpec<String>)>) -> Self {
        let rescale = if content.is_empty() {
            1.0
        } else {
            1.0 / content.iter().map(|e| e.0).sum::<f32>()
        };
        Self {
            content: content
                .into_iter()
                .flat_map(|(p0, loot)| match loot {
                    LootSpec::Item(asset) => vec![(p0 * rescale, asset, 1.0)].into_iter(),
                    LootSpec::ItemQuantity(asset, a, b) => {
                        vec![(p0 * rescale, asset, (a + b) as f32 * 0.5)].into_iter()
                    },
                    LootSpec::LootTable(table_asset) => {
                        let unscaled = &Self::load_expect(&table_asset).read().content;
                        let scale = p0 * rescale;
                        unscaled
                            .iter()
                            .map(|(p1, asset, amount)| (*p1 * scale, asset.clone(), *amount))
                            .collect::<Vec<_>>()
                            .into_iter()
                    },
                    LootSpec::Nothing => Vec::new().into_iter(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TradingPriceFile {
    pub loot_tables: Vec<(f32, bool, String)>,
    // the amount of Good equivalent to the most common item
    pub good_scaling: Vec<(Good, f32)>,
}

impl assets::Asset for TradingPriceFile {
    type Loader = assets::RonLoader;

    const EXTENSION: &'static str = "ron";
}

#[derive(Clone, Debug, Default)]
struct EqualitySet {
    // which item should this item's occurrences be counted towards
    equivalence_class: HashMap<String, String>,
}

impl EqualitySet {
    fn canonical<'a>(&'a self, item_name: &'a str) -> &'a str {
        let canonical_itemname = self
            .equivalence_class
            .get(item_name)
            .map_or(item_name, |i| &**i);

        canonical_itemname
    }
}

impl assets::Compound for EqualitySet {
    fn load<S: assets::source::Source + ?Sized>(
        cache: &assets::AssetCache<S>,
        id: &str,
    ) -> Result<Self, assets::BoxedError> {
        #[derive(Debug, Deserialize)]
        enum EqualitySpec {
            LootTable(String),
            Set(Vec<String>),
        }

        let mut eqset = Self {
            equivalence_class: HashMap::new(),
        };

        let manifest = &cache.load::<assets::Ron<Vec<EqualitySpec>>>(id)?.read().0;
        for set in manifest {
            let items = match set {
                EqualitySpec::LootTable(table) => {
                    let acc = &ProbabilityFile::load_expect(table).read().content;

                    acc.iter().map(|(_p, item, _)| item).cloned().collect()
                },
                EqualitySpec::Set(xs) => xs.clone(),
            };
            let mut iter = items.iter();
            if let Some(first) = iter.next() {
                let first = first.to_string();
                eqset.equivalence_class.insert(first.clone(), first.clone());
                for item in iter {
                    eqset
                        .equivalence_class
                        .insert(item.to_string(), first.clone());
                }
            }
        }
        Ok(eqset)
    }
}

#[derive(Debug)]
struct RememberedRecipe {
    output: String,
    amount: u32,
    material_cost: f32,
    input: Vec<(String, u32)>,
}

fn sort_and_normalize(entryvec: &mut [Entry], scale: f32) {
    if !entryvec.is_empty() {
        entryvec.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal))
        });
        if let Some((_, max_scale, _)) = entryvec.last() {
            // most common item has frequency max_scale.  avoid NaN
            let rescale = scale / max_scale;
            for i in entryvec.iter_mut() {
                i.1 *= rescale;
            }
        }
    }
}

fn get_scaling(contents: &AssetGuard<TradingPriceFile>, good: Good) -> f32 {
    contents
        .good_scaling
        .iter()
        .find(|(good_kind, _)| *good_kind == good)
        .map_or(1.0, |(_, scaling)| *scaling)
}

impl TradePricing {
    const COIN_ITEM: &'static str = "common.items.utility.coins";
    const CRAFTING_FACTOR: f32 = 0.95;
    // increase price a bit compared to sum of ingredients
    const INVEST_FACTOR: f32 = 0.33;
    const UNAVAILABLE_PRICE: f32 = 1_000_000.0;

    // add this much of a non-consumed crafting tool price

    fn get_list(&self, good: Good) -> &[Entry] {
        match good {
            Good::Armor => &self.armor.entries,
            Good::Tools => &self.tools.entries,
            Good::Potions => &self.potions.entries,
            Good::Food => &self.food.entries,
            Good::Ingredients => &self.ingredients.entries,
            _ => &[],
        }
    }

    fn get_list_mut(&mut self, good: Good) -> &mut [Entry] {
        match good {
            Good::Armor => &mut self.armor.entries,
            Good::Tools => &mut self.tools.entries,
            Good::Potions => &mut self.potions.entries,
            Good::Food => &mut self.food.entries,
            Good::Ingredients => &mut self.ingredients.entries,
            _ => &mut [],
        }
    }

    fn get_list_by_path(&self, name: &str) -> &[Entry] {
        match name {
            // Armor
            _ if name.starts_with("common.items.armor.") => &self.armor.entries,
            // Tools
            _ if name.starts_with("common.items.weapons.") => &self.tools.entries,
            _ if name.starts_with("common.items.tool.") => &self.tools.entries,
            // Ingredients
            _ if name.starts_with("common.items.crafting_ing.") => &self.ingredients.entries,
            _ if name.starts_with("common.items.mineral.") => &self.ingredients.entries,
            _ if name.starts_with("common.items.flowers.") => &self.ingredients.entries,
            // Potions
            _ if name.starts_with("common.items.consumable.") => &self.potions.entries,
            // Food
            _ if name.starts_with("common.items.food.") => &self.food.entries,
            // Other
            _ if name.starts_with("common.items.glider.") => &self.other.entries,
            _ if name.starts_with("common.items.utility.") => &self.other.entries,
            _ if name.starts_with("common.items.boss_drops.") => &self.other.entries,
            _ if name.starts_with("common.items.crafting_tools.") => &self.other.entries,
            _ if name.starts_with("common.items.lantern.") => &self.other.entries,
            _ => {
                warn!("unknown loot item {}", name);
                &self.other.entries
            },
        }
    }

    fn get_list_by_path_mut(&mut self, name: &str) -> &mut Entries {
        match name {
            // Armor
            _ if name.starts_with("common.items.armor.") => &mut self.armor,
            // Tools
            _ if name.starts_with("common.items.weapons.") => &mut self.tools,
            _ if name.starts_with("common.items.tool.") => &mut self.tools,
            // Ingredients
            _ if name.starts_with("common.items.crafting_ing.") => &mut self.ingredients,
            _ if name.starts_with("common.items.mineral.") => &mut self.ingredients,
            _ if name.starts_with("common.items.flowers.") => &mut self.ingredients,
            // Potions
            _ if name.starts_with("common.items.consumable.") => &mut self.potions,
            // Food
            _ if name.starts_with("common.items.food.") => &mut self.food,
            // Other
            _ if name.starts_with("common.items.glider.") => &mut self.other,
            _ if name.starts_with("common.items.utility.") => &mut self.other,
            _ if name.starts_with("common.items.boss_drops.") => &mut self.other,
            _ if name.starts_with("common.items.crafting_tools.") => &mut self.other,
            _ if name.starts_with("common.items.lantern.") => &mut self.other,
            _ => {
                warn!("unknown loot item {}", name);
                &mut self.other
            },
        }
    }

    // look up price (inverse frequency) of an item
    fn price_lookup(&self, eqset: &EqualitySet, requested_name: &str) -> f32 {
        let canonical_name = eqset.canonical(requested_name);

        let goods = self.get_list_by_path(canonical_name);
        // even if we multiply by INVEST_FACTOR we need to remain
        // above UNAVAILABLE_PRICE (add 1.0 to compensate rounding errors)
        goods
            .iter()
            .find(|(name, _, _)| name == canonical_name)
            .map_or(
                Self::UNAVAILABLE_PRICE / Self::INVEST_FACTOR + 1.0,
                |(_, freq, _)| 1.0 / freq,
            )
    }

    #[allow(clippy::cast_precision_loss)]
    fn calculate_material_cost(&self, r: &RememberedRecipe, eqset: &EqualitySet) -> f32 {
        r.input
            .iter()
            .map(|(name, amount)| {
                self.price_lookup(eqset, name) * (*amount as f32).max(Self::INVEST_FACTOR)
            })
            .sum()
    }

    // re-look up prices and sort the vector by ascending material cost, return
    // whether first cost is finite
    fn sort_by_price(&self, recipes: &mut Vec<RememberedRecipe>, eqset: &EqualitySet) -> bool {
        for recipe in recipes.iter_mut() {
            recipe.material_cost = self.calculate_material_cost(recipe, eqset);
        }
        recipes.sort_by(|a, b| a.material_cost.partial_cmp(&b.material_cost).unwrap());
        //info!(?recipes);
        recipes
            .first()
            .filter(|recipe| recipe.material_cost < Self::UNAVAILABLE_PRICE)
            .is_some()
    }

    #[allow(clippy::cast_precision_loss)]
    fn read() -> Self {
        let mut result = Self::default();
        let price_config =
            TradingPriceFile::load_expect("common.trading.item_price_calculation").read();
        let eqset = EqualitySet::load_expect("common.trading.item_price_equality").read();
        result.equality_set = eqset.clone();
        for table in &price_config.loot_tables {
            if PRICING_DEBUG {
                info!(?table);
            }
            let (frequency, can_sell, asset_path) = table;
            let loot = ProbabilityFile::load_expect(asset_path);
            for (p, item_asset, amount) in &loot.read().content {
                result.get_list_by_path_mut(item_asset).add(
                    &eqset,
                    item_asset,
                    frequency * p * *amount,
                    *can_sell,
                );
            }
        }

        // Apply recipe book
        let book = default_recipe_book().read();
        let mut ordered_recipes: Vec<RememberedRecipe> = Vec::new();
        for (_, recipe) in book.iter() {
            let (ref asset_path, amount) = recipe.output;
            ordered_recipes.push(RememberedRecipe {
                output: asset_path.id().into(),
                amount,
                material_cost: Self::UNAVAILABLE_PRICE,
                input: recipe
                    .inputs
                    .iter()
                    .filter_map(|&(ref recipe_input, count)| {
                        if let RecipeInput::Item(it) = recipe_input {
                            // If item is not consumed in craft, ignore it
                            if count == 0 {
                                None
                            } else {
                                Some((it.id().into(), count))
                            }
                        } else {
                            None
                        }
                    })
                    .collect(),
            });
        }

        // re-evaluate prices based on crafting tables
        // (start with cheap ones to avoid changing material prices after evaluation)
        while result.sort_by_price(&mut ordered_recipes, &eqset) {
            ordered_recipes.retain(|recipe| {
                if recipe.material_cost < 1e-5 {
                    false
                } else if recipe.material_cost < Self::UNAVAILABLE_PRICE {
                    let actual_cost = result.calculate_material_cost(recipe, &eqset);
                    let output_tradeable = recipe.input.iter().all(|(input, _)| {
                        result
                            .get_list_by_path(input)
                            .iter()
                            .find(|(item, _, _)| item == input)
                            .map_or(false, |(_, _, tradeable)| *tradeable)
                    });
                    result.get_list_by_path_mut(&recipe.output).add(
                        &eqset,
                        &recipe.output,
                        (recipe.amount as f32) / actual_cost * Self::CRAFTING_FACTOR,
                        output_tradeable,
                    );
                    false
                } else {
                    true
                }
            });
            //info!(?ordered_recipes);
        }

        let good_list = [
            Good::Armor,
            Good::Tools,
            Good::Potions,
            Good::Food,
            Good::Ingredients,
        ];

        for good in &good_list {
            sort_and_normalize(
                result.get_list_mut(*good),
                get_scaling(&price_config, *good),
            );
            let mut materials = result
                .get_list(*good)
                .iter()
                .map(|i| (i.0.clone(), (*good, 1.0 / i.1)))
                .collect::<Vec<_>>();
            result.material_cache.extend(materials.drain(..));
        }
        result.coin_scale = get_scaling(&price_config, Good::Coin);
        result
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    fn random_item_impl(&self, good: Good, amount: f32, selling: bool) -> Option<String> {
        if good == Good::Coin {
            Some(Self::COIN_ITEM.into())
        } else {
            let table = self.get_list(good);
            if table.is_empty()
                || (selling && table.iter().filter(|(_, _, can_sell)| *can_sell).count() == 0)
            {
                warn!("Good: {:?}, was unreachable.", good);
                return None;
            }
            let upper = table.len();
            let lower = table
                .iter()
                .enumerate()
                .find(|i| i.1.1 * amount >= 1.0)
                .map_or(upper - 1, |i| i.0);
            loop {
                let index =
                    (rand::random::<f32>() * ((upper - lower) as f32)).floor() as usize + lower;
                if table.get(index).map_or(false, |i| !selling || i.2) {
                    break table.get(index).map(|i| i.0.clone());
                }
            }
        }
    }

    #[must_use]
    pub fn random_item(good: Good, amount: f32, selling: bool) -> Option<String> {
        TRADE_PRICING.random_item_impl(good, amount, selling)
    }

    #[must_use]
    pub fn get_material(item: &str) -> (Good, f32) {
        if item == Self::COIN_ITEM {
            (Good::Coin, 1.0)
        } else {
            let item = TRADE_PRICING.equality_set.canonical(item);

            TRADE_PRICING.material_cache.get(item).copied().map_or(
                (Good::Terrain(crate::terrain::BiomeKind::Void), 0.0),
                |(a, b)| (a, b * TRADE_PRICING.coin_scale),
            )
        }
    }

    #[cfg(test)]
    fn instance() -> &'static Self { &TRADE_PRICING }

    #[cfg(test)]
    fn print_sorted(&self) {
        use crate::comp::item::{armor, tool, Item, ItemKind};

        // we pass the item and the inverse of the price to the closure
        fn printvec<F>(good_kind: &str, entries: &[(String, f32, bool)], f: F, unit: &str)
        where
            F: Fn(&Item, f32) -> String,
        {
            for (item_id, p, can_sell) in entries.iter() {
                let it = Item::new_from_asset_expect(item_id);
                let price = 1.0 / p;
                println!(
                    "{}, {}, {:>4.2}, {}, {:?}, {}, {},",
                    item_id,
                    if *can_sell { "yes" } else { "no" },
                    price,
                    good_kind,
                    it.quality,
                    f(&it, *p),
                    unit,
                );
            }
        }

        println!("Item, ForSale, Amount, Good, Quality, Deal, Unit,");

        printvec(
            "Armor",
            &self.armor.entries,
            |i, p| {
                if let ItemKind::Armor(a) = &i.kind {
                    match a.protection() {
                        Some(armor::Protection::Invincible) => "Invincible".into(),
                        Some(armor::Protection::Normal(x)) => format!("{:.4}", x * p),
                        None => "0.0".into(),
                    }
                } else {
                    format!("{:?}", i.kind)
                }
            },
            "prot/val",
        );
        printvec(
            "Tools",
            &self.tools.entries,
            |i, p| {
                if let ItemKind::Tool(t) = &i.kind {
                    match &t.stats {
                        tool::StatKind::Direct(d) => {
                            format!("{:.4}", d.power * d.speed * p)
                        },
                        tool::StatKind::Modular => "Modular".into(),
                    }
                } else {
                    format!("{:?}", i.kind)
                }
            },
            "dps/val",
        );
        printvec(
            "Potions",
            &self.potions.entries,
            |i, p| {
                if let ItemKind::Consumable { kind: _, effects } = &i.kind {
                    effects
                        .iter()
                        .map(|e| {
                            if let crate::effect::Effect::Buff(b) = e {
                                format!("{:.2}", b.data.strength * p)
                            } else {
                                format!("{:?}", e)
                            }
                        })
                        .collect::<Vec<String>>()
                        .join(" ")
                } else {
                    format!("{:?}", i.kind)
                }
            },
            "str/val",
        );
        printvec(
            "Food",
            &self.food.entries,
            |i, p| {
                if let ItemKind::Consumable { kind: _, effects } = &i.kind {
                    effects
                        .iter()
                        .map(|e| {
                            if let crate::effect::Effect::Buff(b) = e {
                                format!("{:.2}", b.data.strength * p)
                            } else {
                                format!("{:?}", e)
                            }
                        })
                        .collect::<Vec<String>>()
                        .join(" ")
                } else {
                    format!("{:?}", i.kind)
                }
            },
            "str/val",
        );
        printvec(
            "Ingredients",
            &self.ingredients.entries,
            |_i, _p| String::new(),
            "",
        );
        printvec("Other", &self.other.entries, |_i, _p| String::new(), "");
        println!("{}, yes, {}, Coin, ,,,", Self::COIN_ITEM, self.coin_scale);
    }
}

/// hierarchically combine and scale this loot table
#[must_use]
pub fn expand_loot_table(loot_table: &str) -> Vec<(f32, String, f32)> {
    ProbabilityFile::from(vec![(1.0, LootSpec::LootTable(loot_table.into()))]).content
}

// if you want to take a look at the calculated values run:
// cd common && cargo test trade_pricing -- --nocapture
#[cfg(test)]
mod tests {
    use crate::{
        comp::inventory::trade_pricing::{expand_loot_table, ProbabilityFile, TradePricing},
        lottery::LootSpec,
        trade::Good,
    };
    use tracing::{info, Level};
    use tracing_subscriber::{filter::EnvFilter, FmtSubscriber};

    fn init() {
        FmtSubscriber::builder()
            .with_max_level(Level::ERROR)
            .with_env_filter(EnvFilter::from_default_env())
            .try_init()
            .unwrap_or(());
    }

    #[test]
    fn test_loot_table() {
        init();
        info!("init");

        let loot = expand_loot_table("common.loot_tables.creature.quad_medium.gentle");
        let lootsum = loot.iter().fold(0.0, |s, i| s + i.0);
        assert!((lootsum - 1.0).abs() < 1e-3);
        // hierarchical
        let loot2 = expand_loot_table("common.loot_tables.creature.quad_medium.catoblepas");
        let lootsum2 = loot2.iter().fold(0.0, |s, i| s + i.0);
        assert!((lootsum2 - 1.0).abs() < 1e-4);

        // highly nested
        let loot3 = expand_loot_table("common.loot_tables.creature.biped_large.wendigo");
        let lootsum3 = loot3.iter().fold(0.0, |s, i| s + i.0);
        assert!((lootsum3 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_prices1() {
        init();
        info!("init");

        TradePricing::instance().print_sorted();
    }

    #[test]
    fn test_prices2() {
        init();
        info!("init");

        for _ in 0..5 {
            if let Some(item_id) = TradePricing::random_item(Good::Armor, 5.0, false) {
                info!("Armor 5 {}", item_id);
            }
        }
    }

    fn normalized(probability: &ProbabilityFile) -> bool {
        let sum = probability.content.iter().map(|(p, _, _)| p).sum::<f32>();
        (dbg!(sum) - 1.0).abs() < 1e-3
    }

    #[test]
    fn test_normalizing_table1() {
        let item = |asset: &str| LootSpec::Item(asset.to_owned());
        let loot_table = vec![(1.0, item("wow")), (1.0, item("nice"))];

        let probability: ProbabilityFile = loot_table.into();
        assert!(normalized(&probability));
    }

    #[test]
    fn test_normalizing_table2() {
        let table = |asset: &str| LootSpec::LootTable(asset.to_owned());
        let loot_table = vec![(
            1.0,
            table("common.loot_tables.creature.quad_medium.catoblepas"),
        )];
        let probability: ProbabilityFile = loot_table.into();
        assert!(normalized(&probability));
    }

    #[test]
    fn test_normalizing_table3() {
        let table = |asset: &str| LootSpec::LootTable(asset.to_owned());
        let loot_table = vec![
            (
                1.0,
                table("common.loot_tables.creature.quad_medium.catoblepas"),
            ),
            (1.0, table("common.loot_tables.creature.quad_medium.gentle")),
        ];
        let probability: ProbabilityFile = loot_table.into();
        assert!(normalized(&probability));
    }

    #[test]
    fn test_normalizing_table4() {
        let quantity = |asset: &str, a, b| LootSpec::ItemQuantity(asset.to_owned(), a, b);
        let loot_table = vec![(1.0, quantity("such", 3, 5)), (1.0, quantity("much", 5, 9))];
        let probability: ProbabilityFile = loot_table.into();
        assert!(normalized(&probability));
    }
}
