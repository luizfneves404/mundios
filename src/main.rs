use std::collections::HashMap;
use std::env;

use colored::Colorize;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;

type SettlementId = usize;
type RouteId = usize;
type TraderId = usize;

/// Hazard multiplier when the trader's current location is on the route.
const FAMILIARITY_FACTOR: f64 = 0.7;

/// Monthly upkeep per unit of trade capacity, paid to home_base.
const UPKEEP_PER_CAPACITY: f64 = 0.5;

const SIMULATION_MONTHS: u32 = 12;
const DEFAULT_SEED: u64 = 42;

#[derive(Debug, Clone, Copy)]
struct SimulationConfig {
    shortage_effects: bool,
    live_prices: bool,
    stale_prices: bool,
    route_familiarity: bool,
    ruin_aware: bool,
    seed: u64,
    quiet: bool,
}

impl SimulationConfig {
    fn baseline(seed: u64) -> Self {
        Self {
            shortage_effects: false,
            live_prices: false,
            stale_prices: false,
            route_familiarity: false,
            ruin_aware: false,
            seed,
            quiet: false,
        }
    }

    fn named(name: &str, seed: u64, quiet: bool) -> Self {
        let mut config = Self::baseline(seed);
        config.quiet = quiet;
        match name {
            "baseline" => {}
            "shortage-effects" => config.shortage_effects = true,
            "live-prices" => config.live_prices = true,
            "stale-prices" => config.stale_prices = true,
            "route-familiarity" => config.route_familiarity = true,
            "ruin-aware" => config.ruin_aware = true,
            other => panic!(
                "unknown experiment {other:?}; try: baseline, shortage-effects, live-prices, stale-prices, route-familiarity, ruin-aware, compare"
            ),
        }
        config
    }

    fn label(&self) -> &'static str {
        if self.shortage_effects {
            "shortage-effects"
        } else if self.live_prices {
            "live-prices"
        } else if self.stale_prices {
            "stale-prices"
        } else if self.route_familiarity {
            "route-familiarity"
        } else if self.ruin_aware {
            "ruin-aware"
        } else {
            "baseline"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Good {
    Food,
    Ore,
    Tools,
}

#[derive(Debug, Clone)]
struct Settlement {
    id: SettlementId,
    name: String,

    stockpiles: HashMap<Good, f64>,
    prices: HashMap<Good, f64>,

    production: HashMap<Good, f64>,
    consumption: HashMap<Good, f64>,

    money: f64,

    /// Scales next month's production after shortages (1.0 = full output).
    production_scale: HashMap<Good, f64>,
}

#[derive(Debug, Clone)]
struct Route {
    id: RouteId,
    a: SettlementId,
    b: SettlementId,

    travel_months: u32,
    capacity_per_month: f64,
    used_capacity_this_month: f64,

    cost_per_unit: f64,
    /// Per-month probability of a cargo loss event while in transit.
    monthly_incident_rate: f64,
}

#[derive(Debug, Clone)]
struct Trader {
    id: TraderId,
    name: String,

    /// Represents the settlement that you are familiar with.
    /// Reduces hazard on routes that include this settlement.
    location: SettlementId,
    /// Represents the settlement that you hire people from, that you pay cart maintenance to, that you buy food from, etc.
    /// Receives monthly upkeep (fixed for now; no capacity upgrades).
    home_base: SettlementId,

    money: f64,
    /// Monthly pool of trade activity; each shipment uses at least 1 unit.
    trade_capacity: f64,

    /// CRRA coefficient γ — higher values reject risky trades more strongly.
    risk_aversion: f64,

    /// Last observed prices for imperfect-information experiments.
    known_prices: HashMap<(SettlementId, Good), f64>,
    /// Route experience for hazard reduction when route familiarity is enabled.
    route_familiarity: HashMap<RouteId, u32>,
}

#[derive(Debug, Clone)]
struct Shipment {
    trader: TraderId,
    good: Good,
    quantity: f64,
    initial_quantity: f64,
    capacity_units: f64,

    seller: SettlementId,
    buyer: SettlementId,
    route: RouteId,

    buy_price: f64,
    expected_sell_price: f64,
    transport_cost_per_unit: f64,
    /// Frozen at creation so mid-trip location changes do not alter rolls.
    effective_monthly_incident_rate: f64,

    months_remaining: u32,
}

#[derive(Debug, Clone)]
enum Event {
    Produced {
        settlement: SettlementId,
        good: Good,
        amount: f64,
    },
    Consumed {
        settlement: SettlementId,
        good: Good,
        amount: f64,
    },
    Shortage {
        settlement: SettlementId,
        good: Good,
        unmet: f64,
    },
    PriceChanged {
        settlement: SettlementId,
        good: Good,
        old_price: f64,
        new_price: f64,
    },
    UpkeepPaid {
        trader: TraderId,
        home_base: SettlementId,
        amount: f64,
    },
    ShipmentCreated {
        trader: TraderId,
        good: Good,
        quantity: f64,
        seller: SettlementId,
        buyer: SettlementId,
    },
    CargoLost {
        trader: TraderId,
        good: Good,
        lost: f64,
        remaining: f64,
        route: RouteId,
    },
    ShipmentArrived {
        trader: TraderId,
        good: Good,
        quantity: f64,
        initial_quantity: f64,
        seller: SettlementId,
        buyer: SettlementId,
        profit: f64,
    },
}

struct World {
    month: u32,
    expected_total_money: f64,
    config: SimulationConfig,

    settlements: Vec<Settlement>,
    routes: Vec<Route>,
    traders: Vec<Trader>,

    shipments: Vec<Shipment>,
    events: Vec<Event>,
    rng: StdRng,
}

impl World {
    fn tick_month(&mut self) {
        self.month += 1;

        self.reset_route_capacity();
        self.produce();
        self.consume();
        self.update_prices();
        self.pay_trader_upkeep();
        self.create_trade_shipments();
        self.advance_shipments();

        debug_assert!(
            (total_money(self) - self.expected_total_money).abs() < 0.01,
            "money not conserved: got {}, expected {}",
            total_money(self),
            self.expected_total_money
        );
    }

    fn reset_route_capacity(&mut self) {
        for route in &mut self.routes {
            route.used_capacity_this_month = 0.0;
        }
    }

    fn produce(&mut self) {
        for settlement in &mut self.settlements {
            for (&good, &amount) in &settlement.production {
                let scale = production_scale_for(settlement, good, &self.config);
                let produced = amount * scale;
                *settlement.stockpiles.entry(good).or_insert(0.0) += produced;

                self.events.push(Event::Produced {
                    settlement: settlement.id,
                    good,
                    amount: produced,
                });
            }
        }
    }

    fn apply_shortage_effects(
        &mut self,
        settlement_id: SettlementId,
        good: Good,
        needed: f64,
        consumed: f64,
    ) {
        if !self.config.shortage_effects || needed <= 0.0 {
            return;
        }

        let fulfillment = (consumed / needed).clamp(0.0, 1.0);
        let settlement = &mut self.settlements[settlement_id];

        match good {
            Good::Food => {
                for produced_good in settlement.production.keys().copied().collect::<Vec<_>>() {
                    settlement
                        .production_scale
                        .insert(produced_good, fulfillment);
                }
            }
            Good::Ore if settlement_id == 2 => {
                settlement.production_scale.insert(Good::Tools, fulfillment);
            }
            _ => {}
        }
    }

    fn consume(&mut self) {
        let mut shortage_updates = Vec::new();

        for settlement in &mut self.settlements {
            for (&good, &needed) in &settlement.consumption {
                let available = settlement.stockpiles.entry(good).or_insert(0.0);

                let consumed = needed.min(*available);
                *available -= consumed;

                self.events.push(Event::Consumed {
                    settlement: settlement.id,
                    good,
                    amount: consumed,
                });

                if consumed < needed {
                    self.events.push(Event::Shortage {
                        settlement: settlement.id,
                        good,
                        unmet: needed - consumed,
                    });
                    shortage_updates.push((settlement.id, good, needed, consumed));
                }
            }
        }

        for (settlement_id, good, needed, consumed) in shortage_updates {
            self.apply_shortage_effects(settlement_id, good, needed, consumed);
        }
    }

    fn update_prices(&mut self) {
        let base_prices = base_prices();

        for settlement in &mut self.settlements {
            for &good in &[Good::Food, Good::Ore, Good::Tools] {
                let old_price = *settlement.prices.get(&good).unwrap_or(&base_prices[&good]);

                let stockpile = *settlement.stockpiles.get(&good).unwrap_or(&0.0);
                let monthly_need = *settlement.consumption.get(&good).unwrap_or(&1.0);

                let target_stockpile = monthly_need * 3.0;

                let scarcity_ratio = if stockpile <= 0.01 {
                    10.0
                } else {
                    target_stockpile / stockpile
                };

                let scarcity_ratio = scarcity_ratio.clamp(0.25, 10.0);

                let elasticity = 0.7;
                let target_price = base_prices[&good] * scarcity_ratio.powf(elasticity);

                let adjustment_speed = 0.25;
                let new_price =
                    old_price * (1.0 - adjustment_speed) + target_price * adjustment_speed;

                settlement.prices.insert(good, new_price);

                if percentage_change(old_price, new_price).abs() > 0.10 {
                    self.events.push(Event::PriceChanged {
                        settlement: settlement.id,
                        good,
                        old_price,
                        new_price,
                    });
                }
            }
        }

        if self.config.stale_prices {
            self.refresh_trader_price_knowledge();
        }
    }

    fn refresh_trader_price_knowledge(&mut self) {
        for trader in &mut self.traders {
            for settlement_id in 0..self.settlements.len() {
                if settlement_id == trader.location || settlement_id == trader.home_base {
                    for &good in &[Good::Food, Good::Ore, Good::Tools] {
                        trader.known_prices.insert(
                            (settlement_id, good),
                            self.settlements[settlement_id].prices[&good],
                        );
                    }
                }
            }
        }
    }

    fn trader_price(&self, trader_id: TraderId, settlement_id: SettlementId, good: Good) -> f64 {
        if !self.config.stale_prices {
            return self.settlements[settlement_id].prices[&good];
        }

        let trader = &self.traders[trader_id];
        if settlement_id == trader.location || settlement_id == trader.home_base {
            return self.settlements[settlement_id].prices[&good];
        }

        trader
            .known_prices
            .get(&(settlement_id, good))
            .copied()
            .unwrap_or(base_prices()[&good])
    }

    fn pay_trader_upkeep(&mut self) {
        for trader_id in 0..self.traders.len() {
            let upkeep = self.traders[trader_id].trade_capacity * UPKEEP_PER_CAPACITY;
            let home_base = self.traders[trader_id].home_base;
            let payment = upkeep.min(self.traders[trader_id].money);

            self.traders[trader_id].money -= payment;
            self.settlements[home_base].money += payment;

            if payment > 0.0 {
                self.events.push(Event::UpkeepPaid {
                    trader: trader_id,
                    home_base,
                    amount: payment,
                });
            }
        }
    }

    fn create_trade_shipments(&mut self) {
        let trader_count = self.traders.len();

        for trader_id in 0..trader_count {
            let mut remaining_capacity = self.traders[trader_id].trade_capacity;

            while remaining_capacity >= 1.0 {
                let Some(opportunity) = self.best_trade_for_trader(trader_id, remaining_capacity)
                else {
                    break;
                };

                let seller_id = opportunity.seller;
                let buyer_id = opportunity.buyer;
                let good = opportunity.good;
                let buy_price = self.settlements[seller_id].prices[&good];
                let buyer_price = opportunity.buyer_price;

                let seller_stock = self.settlements[seller_id]
                    .stockpiles
                    .get(&good)
                    .copied()
                    .unwrap_or(0.0);
                let trader_money = self.traders[trader_id].money;
                let buyer_money = self.settlements[buyer_id].money;

                let affordable_by_trader = trader_money / buy_price;
                let affordable_by_buyer = buyer_money / buyer_price;
                let route_capacity_left = self.routes[opportunity.route].capacity_per_month
                    - self.routes[opportunity.route].used_capacity_this_month;

                let quantity = opportunity
                    .suggested_quantity
                    .min(seller_stock)
                    .min(affordable_by_trader)
                    .min(affordable_by_buyer)
                    .min(route_capacity_left)
                    .min(remaining_capacity)
                    .floor();

                if quantity < 1.0 {
                    break;
                }

                let purchase_cost = quantity * buy_price;

                self.traders[trader_id].money -= purchase_cost;
                self.settlements[seller_id].money += purchase_cost;
                *self.settlements[seller_id]
                    .stockpiles
                    .entry(good)
                    .or_insert(0.0) -= quantity;
                self.routes[opportunity.route].used_capacity_this_month += quantity;

                let route = &self.routes[opportunity.route];
                let effective_rate = if self.config.route_familiarity {
                    effective_monthly_incident_rate_for_trader(route, trader_id, self)
                } else {
                    effective_monthly_incident_rate(route, self.traders[trader_id].location)
                };

                let shipment = Shipment {
                    trader: trader_id,
                    good,
                    quantity,
                    initial_quantity: quantity,
                    capacity_units: quantity,
                    seller: seller_id,
                    buyer: buyer_id,
                    route: opportunity.route,
                    buy_price,
                    expected_sell_price: buyer_price,
                    transport_cost_per_unit: route.cost_per_unit,
                    effective_monthly_incident_rate: effective_rate,
                    months_remaining: route.travel_months,
                };

                self.shipments.push(shipment);
                remaining_capacity -= quantity;

                self.events.push(Event::ShipmentCreated {
                    trader: trader_id,
                    good,
                    quantity,
                    seller: seller_id,
                    buyer: buyer_id,
                });

                if self.config.live_prices {
                    self.update_prices();
                }
            }
        }
    }

    fn best_trade_for_trader(
        &self,
        trader_id: TraderId,
        max_capacity: f64,
    ) -> Option<TradeOpportunity> {
        let trader = &self.traders[trader_id];
        let mut best: Option<TradeOpportunity> = None;

        for route in &self.routes {
            let orientations = [(route.a, route.b), (route.b, route.a)];

            for &(seller_id, buyer_id) in &orientations {
                let seller = &self.settlements[seller_id];
                let buyer = &self.settlements[buyer_id];

                for &good in &[Good::Food, Good::Ore, Good::Tools] {
                    let seller_price = self.trader_price(trader_id, seller_id, good);
                    let buyer_price = self.trader_price(trader_id, buyer_id, good);

                    let p_eff = if self.config.route_familiarity {
                        effective_monthly_incident_rate_for_trader(route, trader_id, self)
                    } else {
                        effective_monthly_incident_rate(route, trader.location)
                    };

                    let ev_per_unit = expected_profit_per_unit(
                        seller_price,
                        buyer_price,
                        route.cost_per_unit,
                        p_eff,
                        route.travel_months,
                    );

                    if ev_per_unit <= 0.0 {
                        continue;
                    }

                    let stock = *seller.stockpiles.get(&good).unwrap_or(&0.0);
                    let buyer_need = buyer.consumption.get(&good).copied().unwrap_or(1.0) * 3.0;
                    let buyer_stock = *buyer.stockpiles.get(&good).unwrap_or(&0.0);
                    let shortage = (buyer_need - buyer_stock).max(0.0);

                    let suggested_quantity = stock.min(shortage).min(max_capacity).floor();

                    if suggested_quantity < 1.0 {
                        continue;
                    }

                    if self.config.ruin_aware {
                        let purchase = suggested_quantity * seller_price;
                        let transport = suggested_quantity * route.cost_per_unit;
                        let w_bad = trader.money - purchase - transport;
                        if w_bad <= 0.0 {
                            continue;
                        }
                    }

                    let score = trade_utility_score(
                        trader.money,
                        suggested_quantity,
                        seller_price,
                        buyer_price,
                        route.cost_per_unit,
                        p_eff,
                        route.travel_months,
                        trader.risk_aversion,
                    );

                    if score <= 0.0 {
                        continue;
                    }

                    let opportunity = TradeOpportunity {
                        seller: seller_id,
                        buyer: buyer_id,
                        route: route.id,
                        good,
                        buyer_price,
                        suggested_quantity,
                        score,
                    };

                    if best.as_ref().map_or(true, |b| opportunity.score > b.score) {
                        best = Some(opportunity);
                    }
                }
            }
        }

        best
    }

    fn advance_shipments(&mut self) {
        let mut remaining_shipments = Vec::new();

        for mut shipment in self.shipments.drain(..) {
            if shipment.months_remaining > 0 {
                if let Some(lost) = apply_monthly_cargo_risk(
                    &mut shipment.quantity,
                    shipment.effective_monthly_incident_rate,
                    &mut self.rng,
                ) {
                    self.events.push(Event::CargoLost {
                        trader: shipment.trader,
                        good: shipment.good,
                        lost,
                        remaining: shipment.quantity,
                        route: shipment.route,
                    });
                }

                shipment.months_remaining -= 1;
            }

            if shipment.months_remaining > 0 {
                remaining_shipments.push(shipment);
                continue;
            }

            let sell_price = self.settlements[shipment.buyer].prices[&shipment.good];
            let route = &self.routes[shipment.route];

            let revenue = shipment.quantity * sell_price;
            let purchase_cost = shipment.initial_quantity * shipment.buy_price;
            let transport_cost = shipment.quantity * shipment.transport_cost_per_unit;
            let transport_half = transport_cost / 2.0;

            let profit = revenue - purchase_cost - transport_cost;

            self.settlements[shipment.buyer]
                .stockpiles
                .entry(shipment.good)
                .and_modify(|q| *q += shipment.quantity)
                .or_insert(shipment.quantity);

            self.settlements[shipment.buyer].money -= revenue;
            self.traders[shipment.trader].money += revenue;
            self.traders[shipment.trader].money -= transport_cost;
            self.settlements[route.a].money += transport_half;
            self.settlements[route.b].money += transport_half;

            self.traders[shipment.trader].location = shipment.buyer;

            if self.config.route_familiarity {
                *self.traders[shipment.trader]
                    .route_familiarity
                    .entry(shipment.route)
                    .or_insert(0) += 1;
            }

            if self.config.stale_prices {
                let trader_id = shipment.trader;
                let buyer = shipment.buyer;
                for &good in &[Good::Food, Good::Ore, Good::Tools] {
                    self.traders[trader_id]
                        .known_prices
                        .insert((buyer, good), self.settlements[buyer].prices[&good]);
                }
            }

            self.events.push(Event::ShipmentArrived {
                trader: shipment.trader,
                good: shipment.good,
                quantity: shipment.quantity,
                initial_quantity: shipment.initial_quantity,
                seller: shipment.seller,
                buyer: shipment.buyer,
                profit,
            });
        }

        self.shipments = remaining_shipments;
    }
}

#[derive(Debug, Clone)]
struct TradeOpportunity {
    seller: SettlementId,
    buyer: SettlementId,
    route: RouteId,
    good: Good,
    buyer_price: f64,
    suggested_quantity: f64,
    score: f64,
}

fn effective_monthly_incident_rate(route: &Route, trader_location: SettlementId) -> f64 {
    let base = route.monthly_incident_rate;
    if trader_location == route.a || trader_location == route.b {
        base * FAMILIARITY_FACTOR
    } else {
        base
    }
}

fn effective_monthly_incident_rate_for_trader(
    route: &Route,
    trader_id: TraderId,
    world: &World,
) -> f64 {
    let base = route.monthly_incident_rate;
    let trips = world.traders[trader_id]
        .route_familiarity
        .get(&route.id)
        .copied()
        .unwrap_or(0);
    let familiarity = 1.0 - (trips as f64 * 0.15).min(0.5);
    base * familiarity
}

fn production_scale_for(settlement: &Settlement, good: Good, config: &SimulationConfig) -> f64 {
    if !config.shortage_effects {
        return 1.0;
    }
    *settlement.production_scale.get(&good).unwrap_or(&1.0)
}

/// E[fraction delivered] with at most one incident per month and U[0,1] loss fraction.
fn expected_delivery_fraction(p_eff: f64, travel_months: u32) -> f64 {
    (1.0 - 0.5 * p_eff).powi(travel_months as i32)
}

/// Risk-neutral expected profit per unit (rational benchmark).
fn expected_profit_per_unit(
    seller_price: f64,
    buyer_price: f64,
    cost_per_unit: f64,
    p_eff: f64,
    travel_months: u32,
) -> f64 {
    let frac = expected_delivery_fraction(p_eff, travel_months);
    frac * (buyer_price - seller_price - cost_per_unit) - (1.0 - frac) * seller_price
}

fn crra_utility(wealth: f64, gamma: f64) -> f64 {
    if wealth <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if (gamma - 1.0).abs() < 1e-9 {
        wealth.ln()
    } else {
        wealth.powf(1.0 - gamma) / (1.0 - gamma)
    }
}

/// Decision score: E[U(wealth after trade)] - U(current wealth).
fn trade_utility_score(
    current_money: f64,
    quantity: f64,
    buy_price: f64,
    sell_price: f64,
    transport_per_unit: f64,
    p_eff: f64,
    travel_months: u32,
    gamma: f64,
) -> f64 {
    let purchase = quantity * buy_price;
    let transport = quantity * transport_per_unit;
    let frac_ev = expected_delivery_fraction(p_eff, travel_months);

    let p_good = (1.0 - p_eff).powi(travel_months as i32);
    let p_bad = (1.0 - p_good) * 0.5;
    let p_ev = (1.0 - p_good - p_bad).max(0.0);

    let w_good = current_money - purchase - transport + quantity * sell_price;
    let w_bad = current_money - purchase - transport;
    let w_ev = current_money - purchase - transport + quantity * frac_ev * sell_price;

    let u_current = crra_utility(current_money, gamma);
    p_good * crra_utility(w_good, gamma)
        + p_ev * crra_utility(w_ev, gamma)
        + p_bad * crra_utility(w_bad, gamma)
        - u_current
}

fn apply_monthly_cargo_risk(
    quantity: &mut f64,
    monthly_incident_rate: f64,
    rng: &mut impl Rng,
) -> Option<f64> {
    if rng.gen_range(0.0..1.0) >= monthly_incident_rate {
        return None;
    }

    let loss_fraction: f64 = rng.gen_range(0.0..=1.0);
    let lost = *quantity * loss_fraction;
    *quantity -= lost;
    Some(lost)
}

fn total_money(world: &World) -> f64 {
    let settlement_money: f64 = world.settlements.iter().map(|s| s.money).sum();
    let trader_money: f64 = world.traders.iter().map(|t| t.money).sum();
    settlement_money + trader_money
}

fn base_prices() -> HashMap<Good, f64> {
    HashMap::from([(Good::Food, 10.0), (Good::Ore, 8.0), (Good::Tools, 20.0)])
}

fn percentage_change(old: f64, new: f64) -> f64 {
    if old.abs() < 0.0001 {
        0.0
    } else {
        (new - old) / old
    }
}

fn good_name(good: Good) -> &'static str {
    match good {
        Good::Food => "Food",
        Good::Ore => "Ore",
        Good::Tools => "Tools",
    }
}

fn good_color(good: Good) -> Color {
    match good {
        Good::Food => Color::Green,
        Good::Ore => Color::Yellow,
        Good::Tools => Color::Cyan,
    }
}

fn header_cell(label: &str) -> Cell {
    Cell::new(label)
        .fg(Color::Cyan)
        .add_attribute(Attribute::Bold)
}

fn place_cell(name: &str) -> Cell {
    Cell::new(name)
        .add_attribute(Attribute::Bold)
        .set_alignment(CellAlignment::Left)
}

fn good_cell(good: Good) -> Cell {
    Cell::new(good_name(good))
        .fg(good_color(good))
        .set_alignment(CellAlignment::Left)
}

fn num_cell(value: &str) -> Cell {
    Cell::new(value).set_alignment(CellAlignment::Right)
}

fn format_price_change(old: f64, new: f64) -> (String, Color) {
    let pct = percentage_change(old, new) * 100.0;
    let arrow = if new >= old { "↑" } else { "↓" };
    let (pct_color, sign) = if pct >= 0.0 {
        (Color::Green, "+")
    } else {
        (Color::Red, "")
    };
    (
        format!("{old:.2} → {new:.2} ({arrow} {sign}{pct:.1}%)"),
        pct_color,
    )
}

struct EventRow {
    kind: &'static str,
    kind_color: Color,
    kind_bold: bool,
    place: String,
    good: Option<Good>,
    details: String,
    details_color: Option<Color>,
}

fn format_event(event: &Event, world: &World) -> EventRow {
    let settlement_name = |id: SettlementId| world.settlements[id].name.clone();

    match event {
        Event::Produced {
            settlement,
            good,
            amount,
        } => EventRow {
            kind: "Produced",
            kind_color: Color::Green,
            kind_bold: false,
            place: settlement_name(*settlement),
            good: Some(*good),
            details: format!("+{amount:.1}"),
            details_color: Some(Color::Green),
        },
        Event::Consumed {
            settlement,
            good,
            amount,
        } => EventRow {
            kind: "Consumed",
            kind_color: Color::Blue,
            kind_bold: false,
            place: settlement_name(*settlement),
            good: Some(*good),
            details: format!("-{amount:.1}"),
            details_color: None,
        },
        Event::Shortage {
            settlement,
            good,
            unmet,
        } => EventRow {
            kind: "Shortage",
            kind_color: Color::Red,
            kind_bold: true,
            place: settlement_name(*settlement),
            good: Some(*good),
            details: format!("unmet {unmet:.1}"),
            details_color: Some(Color::Red),
        },
        Event::PriceChanged {
            settlement,
            good,
            old_price,
            new_price,
        } => {
            let (details, details_color) = format_price_change(*old_price, *new_price);
            EventRow {
                kind: "Price",
                kind_color: Color::Magenta,
                kind_bold: false,
                place: settlement_name(*settlement),
                good: Some(*good),
                details,
                details_color: Some(details_color),
            }
        }
        Event::UpkeepPaid {
            trader,
            home_base,
            amount,
        } => EventRow {
            kind: "Upkeep",
            kind_color: Color::Yellow,
            kind_bold: false,
            place: format!(
                "{} → {}",
                world.traders[*trader].name,
                settlement_name(*home_base)
            ),
            good: None,
            details: format!("-{amount:.1}"),
            details_color: Some(Color::Yellow),
        },
        Event::ShipmentCreated {
            trader,
            good,
            quantity,
            seller,
            buyer,
        } => EventRow {
            kind: "Shipment",
            kind_color: Color::Cyan,
            kind_bold: false,
            place: format!(
                "{}: {} → {}",
                world.traders[*trader].name,
                settlement_name(*seller),
                settlement_name(*buyer)
            ),
            good: Some(*good),
            details: format!("{quantity:.0} units"),
            details_color: None,
        },
        Event::CargoLost {
            trader,
            good,
            lost,
            remaining,
            route: _,
        } => EventRow {
            kind: "CargoLost",
            kind_color: Color::Red,
            kind_bold: true,
            place: world.traders[*trader].name.clone(),
            good: Some(*good),
            details: format!("-{lost:.1}, {remaining:.1} left"),
            details_color: Some(Color::Red),
        },
        Event::ShipmentArrived {
            trader,
            good,
            quantity,
            initial_quantity,
            seller,
            buyer,
            profit,
        } => {
            let (profit_text, profit_color) = if *profit >= 0.0 {
                (format!("+{profit:.1}"), Color::Green)
            } else {
                (format!("{profit:.1}"), Color::Red)
            };
            let qty_text = if (initial_quantity - quantity).abs() > 0.01 {
                format!("{quantity:.0}/{initial_quantity:.0} ({profit_text})")
            } else {
                format!("{quantity:.0} ({profit_text})")
            };
            EventRow {
                kind: "Arrived",
                kind_color: Color::Green,
                kind_bold: true,
                place: format!(
                    "{}: {} → {}",
                    world.traders[*trader].name,
                    settlement_name(*seller),
                    settlement_name(*buyer)
                ),
                good: Some(*good),
                details: qty_text,
                details_color: Some(profit_color),
            }
        }
    }
}

fn event_kind_cell(row: &EventRow) -> Cell {
    let mut cell = Cell::new(row.kind).fg(row.kind_color);
    if row.kind_bold {
        cell = cell.add_attribute(Attribute::Bold);
    }
    cell.set_alignment(CellAlignment::Left)
}

fn event_details_cell(row: &EventRow) -> Cell {
    let mut cell = Cell::new(&row.details).set_alignment(CellAlignment::Left);
    if let Some(color) = row.details_color {
        cell = cell.fg(color);
    }
    cell
}

fn styled_table() -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table
}

fn print_events(events: &[Event], world: &World) {
    if events.is_empty() {
        println!("{}", "  (no events this month)".dimmed());
        return;
    }

    let mut table = styled_table();
    table.set_header(vec![
        header_cell("Type"),
        header_cell("Where / Who"),
        header_cell("Good"),
        header_cell("Details"),
    ]);

    for event in events {
        let row = format_event(event, world);
        table.add_row(vec![
            event_kind_cell(&row),
            place_cell(&row.place),
            match row.good {
                Some(g) => good_cell(g),
                None => Cell::new("—").set_alignment(CellAlignment::Left),
            },
            event_details_cell(&row),
        ]);
    }

    println!("{table}");
}

fn print_settlements(settlements: &[Settlement]) {
    let mut table = styled_table();
    table.set_header(vec![
        header_cell("Settlement"),
        header_cell("Money"),
        header_cell("Food").fg(Color::Green),
        header_cell("Ore").fg(Color::Yellow),
        header_cell("Tools").fg(Color::Cyan),
        header_cell("Food $").fg(Color::Green),
        header_cell("Ore $").fg(Color::Yellow),
        header_cell("Tools $").fg(Color::Cyan),
    ]);

    for s in settlements {
        table.add_row(vec![
            place_cell(&s.name),
            num_cell(&format!("{:.1}", s.money)).fg(Color::Green),
            num_cell(&format!(
                "{:.1}",
                s.stockpiles.get(&Good::Food).unwrap_or(&0.0)
            )),
            num_cell(&format!(
                "{:.1}",
                s.stockpiles.get(&Good::Ore).unwrap_or(&0.0)
            )),
            num_cell(&format!(
                "{:.1}",
                s.stockpiles.get(&Good::Tools).unwrap_or(&0.0)
            )),
            num_cell(&format!("{:.2}", s.prices.get(&Good::Food).unwrap_or(&0.0))),
            num_cell(&format!("{:.2}", s.prices.get(&Good::Ore).unwrap_or(&0.0))),
            num_cell(&format!(
                "{:.2}",
                s.prices.get(&Good::Tools).unwrap_or(&0.0)
            )),
        ]);
    }

    println!("{table}");
}

fn print_traders(traders: &[Trader], settlements: &[Settlement]) {
    let mut table = styled_table();
    table.set_header(vec![
        header_cell("Trader"),
        header_cell("Location"),
        header_cell("Home"),
        header_cell("Money"),
        header_cell("Capacity"),
        header_cell("Risk γ"),
    ]);

    for t in traders {
        table.add_row(vec![
            place_cell(&t.name),
            Cell::new(&settlements[t.location].name).set_alignment(CellAlignment::Left),
            Cell::new(&settlements[t.home_base].name).set_alignment(CellAlignment::Left),
            num_cell(&format!("{:.1}", t.money)).fg(Color::Green),
            num_cell(&format!("{:.0}/mo", t.trade_capacity)),
            num_cell(&format!("{:.1}", t.risk_aversion)),
        ]);
    }

    println!("{table}");
}

#[derive(Debug)]
struct SimulationSummary {
    experiment: String,
    shipments_created: u32,
    shipments_arrived: u32,
    cargo_losses: u32,
    shortages: u32,
    price_changes: u32,
    months_with_trade: u32,
    last_month_with_trade: u32,
    trader_money_spread: f64,
    min_settlement_money: f64,
    total_shortage_unmet: f64,
    trader_money: Vec<f64>,
}

impl SimulationSummary {
    fn from_events(
        experiment: &str,
        events: &[Event],
        world: &World,
        months_with_trade: u32,
        last_month_with_trade: u32,
    ) -> Self {
        let mut shipments_created = 0;
        let mut shipments_arrived = 0;
        let mut cargo_losses = 0;
        let mut shortages = 0;
        let mut price_changes = 0;
        let mut total_shortage_unmet = 0.0;

        for event in events {
            match event {
                Event::ShipmentCreated { .. } => shipments_created += 1,
                Event::ShipmentArrived { .. } => shipments_arrived += 1,
                Event::CargoLost { .. } => cargo_losses += 1,
                Event::Shortage { unmet, .. } => {
                    shortages += 1;
                    total_shortage_unmet += unmet;
                }
                Event::PriceChanged { .. } => price_changes += 1,
                _ => {}
            }
        }

        let trader_money: Vec<f64> = world.traders.iter().map(|t| t.money).collect();
        let trader_money_spread = trader_money.iter().copied().reduce(f64::max).unwrap_or(0.0)
            - trader_money.iter().copied().reduce(f64::min).unwrap_or(0.0);
        let min_settlement_money = world
            .settlements
            .iter()
            .map(|s| s.money)
            .reduce(f64::min)
            .unwrap_or(0.0);

        Self {
            experiment: experiment.to_string(),
            shipments_created,
            shipments_arrived,
            cargo_losses,
            shortages,
            price_changes,
            months_with_trade,
            last_month_with_trade,
            trader_money_spread,
            min_settlement_money,
            total_shortage_unmet,
            trader_money,
        }
    }
}

fn run_simulation(config: SimulationConfig) -> SimulationSummary {
    let mut world = create_demo_world(config);
    let mut all_events = Vec::new();
    let mut months_with_trade = 0;
    let mut last_month_with_trade = 0;

    for _ in 0..SIMULATION_MONTHS {
        world.tick_month();
        let month_events: Vec<Event> = world.events.drain(..).collect();
        if month_events
            .iter()
            .any(|e| matches!(e, Event::ShipmentCreated { .. }))
        {
            months_with_trade += 1;
            last_month_with_trade = world.month;
        }
        all_events.extend(month_events);
    }

    SimulationSummary::from_events(
        config.label(),
        &all_events,
        &world,
        months_with_trade,
        last_month_with_trade,
    )
}

fn run_simulation_verbose(config: SimulationConfig) -> SimulationSummary {
    let mut world = create_demo_world(config);
    let mut all_events = Vec::new();
    let mut months_with_trade = 0;
    let mut last_month_with_trade = 0;

    println!(
        "{}",
        format!("Experiment: {}", config.label()).bold().cyan()
    );
    println!("{}", "Initial state:".bold().cyan());
    print_settlements(&world.settlements);
    println!();
    print_traders(&world.traders, &world.settlements);
    println!();

    for _ in 0..SIMULATION_MONTHS {
        world.tick_month();

        println!(
            "\n{}",
            format!("═══ Month {} ═══", world.month).bold().cyan()
        );

        let month_events: Vec<Event> = world.events.drain(..).collect();
        if month_events
            .iter()
            .any(|e| matches!(e, Event::ShipmentCreated { .. }))
        {
            months_with_trade += 1;
            last_month_with_trade = world.month;
        }
        print_events(&month_events, &world);
        all_events.extend(month_events);
        println!();
        print_settlements(&world.settlements);
        println!();
        print_traders(&world.traders, &world.settlements);
    }

    SimulationSummary::from_events(
        config.label(),
        &all_events,
        &world,
        months_with_trade,
        last_month_with_trade,
    )
}

fn print_comparison_table(summaries: &[SimulationSummary]) {
    let mut table = styled_table();
    table.set_header(vec![
        header_cell("Experiment"),
        header_cell("Shipments"),
        header_cell("Arrived"),
        header_cell("CargoLost"),
        header_cell("Shortages"),
        header_cell("Price Δ"),
        header_cell("Trade mo"),
        header_cell("Last trade"),
        header_cell("Trader spread"),
        header_cell("Min settlement $"),
        header_cell("Unmet demand"),
    ]);

    for s in summaries {
        table.add_row(vec![
            place_cell(&s.experiment),
            num_cell(&s.shipments_created.to_string()),
            num_cell(&s.shipments_arrived.to_string()),
            num_cell(&s.cargo_losses.to_string()),
            num_cell(&s.shortages.to_string()),
            num_cell(&s.price_changes.to_string()),
            num_cell(&format!("{}/{}", s.months_with_trade, SIMULATION_MONTHS)),
            num_cell(&s.last_month_with_trade.to_string()),
            num_cell(&format!("{:.1}", s.trader_money_spread)),
            num_cell(&format!("{:.1}", s.min_settlement_money)),
            num_cell(&format!("{:.0}", s.total_shortage_unmet)),
        ]);
    }

    println!("{table}");
    println!();
    for s in summaries {
        let money: Vec<String> = s.trader_money.iter().map(|m| format!("{m:.1}")).collect();
        println!(
            "  {} → trader money [{}], trade died month {}",
            s.experiment,
            money.join(", "),
            if s.last_month_with_trade == 0 {
                "never".to_string()
            } else if s.last_month_with_trade < SIMULATION_MONTHS {
                s.last_month_with_trade.to_string()
            } else {
                "still active".to_string()
            }
        );
    }
}

fn parse_args() -> (String, u64, bool) {
    let mut args = env::args().skip(1);
    let mut experiment = "baseline".to_string();
    let mut seed = DEFAULT_SEED;
    let mut quiet = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--experiment" | "-e" => {
                experiment = args.next().expect("missing value for --experiment");
            }
            "--seed" => {
                seed = args
                    .next()
                    .expect("missing value for --seed")
                    .parse()
                    .expect("seed must be a u64");
            }
            "--quiet" | "-q" => quiet = true,
            "--compare" => experiment = "compare".to_string(),
            "--help" | "-h" => {
                println!(
                    "Usage: mundios [--experiment NAME] [--seed N] [--quiet] [--compare]\n\
                     Experiments: baseline, shortage-effects, live-prices, stale-prices, \
                     route-familiarity, ruin-aware, compare"
                );
                std::process::exit(0);
            }
            other => {
                experiment = other.to_string();
            }
        }
    }

    (experiment, seed, quiet)
}

fn main() {
    let (experiment, seed, quiet) = parse_args();

    if experiment == "compare" {
        let names = [
            "baseline",
            "shortage-effects",
            "live-prices",
            "stale-prices",
            "route-familiarity",
            "ruin-aware",
        ];
        let summaries: Vec<SimulationSummary> = names
            .iter()
            .map(|name| {
                let config = SimulationConfig::named(name, seed, true);
                run_simulation(config)
            })
            .collect();
        print_comparison_table(&summaries);
        return;
    }

    let config = SimulationConfig::named(&experiment, seed, quiet);
    if quiet {
        let summary = run_simulation(config);
        println!(
            "{}: shipments={} arrived={} shortages={} trade_months={}/{} last_trade={} trader_spread={:.1} min_settlement=${:.1}",
            summary.experiment,
            summary.shipments_created,
            summary.shipments_arrived,
            summary.shortages,
            summary.months_with_trade,
            SIMULATION_MONTHS,
            summary.last_month_with_trade,
            summary.trader_money_spread,
            summary.min_settlement_money,
        );
    } else {
        run_simulation_verbose(config);
    }
}

fn create_demo_world(config: SimulationConfig) -> World {
    let base = base_prices();

    let farm_world = Settlement {
        id: 0,
        name: "Greenworld".to_string(),
        stockpiles: HashMap::from([(Good::Food, 500.0), (Good::Ore, 20.0), (Good::Tools, 30.0)]),
        prices: base.clone(),
        production: HashMap::from([(Good::Food, 150.0)]),
        consumption: HashMap::from([(Good::Food, 80.0), (Good::Tools, 10.0)]),
        money: 1000.0,
        production_scale: HashMap::new(),
    };

    let mining_world = Settlement {
        id: 1,
        name: "Ironmoon".to_string(),
        stockpiles: HashMap::from([(Good::Food, 50.0), (Good::Ore, 300.0), (Good::Tools, 20.0)]),
        prices: base.clone(),
        production: HashMap::from([(Good::Ore, 120.0)]),
        consumption: HashMap::from([(Good::Food, 120.0), (Good::Tools, 15.0)]),
        money: 1000.0,
        production_scale: HashMap::new(),
    };

    let factory_world = Settlement {
        id: 2,
        name: "Forge Prime".to_string(),
        stockpiles: HashMap::from([(Good::Food, 100.0), (Good::Ore, 50.0), (Good::Tools, 100.0)]),
        prices: base.clone(),
        production: HashMap::from([(Good::Tools, 60.0)]),
        consumption: HashMap::from([(Good::Food, 140.0), (Good::Ore, 90.0)]),
        money: 1000.0,
        production_scale: HashMap::new(),
    };

    let routes = vec![
        Route {
            id: 0,
            a: 0,
            b: 1,
            travel_months: 1,
            capacity_per_month: 100.0,
            used_capacity_this_month: 0.0,
            cost_per_unit: 2.0,
            monthly_incident_rate: 0.05,
        },
        Route {
            id: 1,
            a: 1,
            b: 2,
            travel_months: 2,
            capacity_per_month: 100.0,
            used_capacity_this_month: 0.0,
            cost_per_unit: 2.0,
            monthly_incident_rate: 0.08,
        },
        Route {
            id: 2,
            a: 2,
            b: 0,
            travel_months: 1,
            capacity_per_month: 100.0,
            used_capacity_this_month: 0.0,
            cost_per_unit: 2.0,
            monthly_incident_rate: 0.05,
        },
    ];

    let traders = vec![
        Trader {
            id: 0,
            name: "Free Merchants".to_string(),
            location: 0,
            home_base: 0,
            money: 1000.0,
            trade_capacity: 80.0,
            risk_aversion: 2.0,
            known_prices: HashMap::new(),
            route_familiarity: HashMap::new(),
        },
        Trader {
            id: 1,
            name: "Ironmoon Haulers".to_string(),
            location: 1,
            home_base: 1,
            money: 1000.0,
            trade_capacity: 80.0,
            risk_aversion: 0.5,
            known_prices: HashMap::new(),
            route_familiarity: HashMap::new(),
        },
    ];

    let settlements = vec![farm_world, mining_world, factory_world];

    let mut world = World {
        month: 0,
        expected_total_money: 0.0,
        config,
        settlements,
        routes,
        traders,
        shipments: Vec::new(),
        events: Vec::new(),
        rng: StdRng::seed_from_u64(config.seed),
    };
    world.refresh_trader_price_knowledge();
    world.expected_total_money = total_money(&world);
    world
}
