use std::collections::HashMap;
use std::env;

use colored::Colorize;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};
use rand::Rng;

type SettlementId = usize;
type RouteId = usize;
type TraderId = usize;

/// Hazard multiplier when the trader's current location is on the route.
const FAMILIARITY_FACTOR: f64 = 0.7;

/// Monthly upkeep per unit of trade capacity, paid to home_base.
const UPKEEP_PER_CAPACITY: f64 = 0.5;
/// Crew, supplies, and services bought at the trader's current settlement.
const LOCAL_SPEND_PER_CAPACITY: f64 = 0.5;
/// Fraction of trader wealth above reserve spent locally each month.
const TRADER_REPATRIATION_RATE: f64 = 0.15;
/// Minimum cash traders keep for operating the next month's routes.
const TRADER_OPERATING_RESERVE: f64 = 400.0;
/// Settlements may borrow against near-term production while waiting for export revenue.
const CREDIT_MONTHS_OF_PRODUCTION: f64 = 3.0;
/// Traders cannot keep more than this fraction of import revenue as profit.
const MAX_TRADER_MARGIN: f64 = 0.15;
/// Monthly interest charged on settlement debt (paid to creditors with positive balances).
const DEBT_INTEREST_RATE: f64 = 0.02;
/// At maximum debt stress, consumption is cut by this fraction (austerity).
const MAX_AUSTERITY: f64 = 0.5;
/// Above this treasury balance, settlements gradually increase consumption (prosperity spending).
const PROSPERITY_THRESHOLD: f64 = 2500.0;
/// How quickly production recovers toward full output after shortages ease.
const PRODUCTION_RECOVERY_RATE: f64 = 0.2;

/// Target stockpile (in months of consumption) that triggers import demand.
const STOCK_TARGET_MONTHS: f64 = 1.5;
/// Minimum per-unit spread (sell - buy - transport) a trader wants to see.
const MIN_HEURISTIC_SPREAD: f64 = 0.5;
/// How wrong one-hop market rumors can be (± fraction).
const RUMOR_NOISE: f64 = 0.1;
const DEFAULT_MONTHS: u32 = 12;

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

    /// Scales output after shortages (1.0 = full capacity).
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

    /// 0–1: willingness to run routes with cargo risk (higher = bolder).
    risk_tolerance: f64,

    /// Last observed prices; fresh at current location and home base only.
    known_prices: HashMap<(SettlementId, Good), f64>,
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
    LocalSpending {
        trader: TraderId,
        settlement: SettlementId,
        amount: f64,
    },
    DebtInterest {
        debtor: SettlementId,
        amount: f64,
    },
    Austerity {
        settlement: SettlementId,
        multiplier: f64,
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

    settlements: Vec<Settlement>,
    routes: Vec<Route>,
    traders: Vec<Trader>,

    shipments: Vec<Shipment>,
    events: Vec<Event>,
}

impl World {
    fn tick_month(&mut self) {
        self.month += 1;

        self.reset_route_capacity();
        self.recover_production_capacity();
        self.produce();
        self.consume();
        self.update_prices();
        self.service_debt_interest();
        self.pay_trader_expenses();
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

    fn recover_production_capacity(&mut self) {
        for settlement in &mut self.settlements {
            for good in [Good::Food, Good::Ore, Good::Tools] {
                let scale = settlement.production_scale.entry(good).or_insert(1.0);
                if *scale < 1.0 {
                    *scale = (*scale + PRODUCTION_RECOVERY_RATE).min(1.0);
                }
            }
        }
    }

    fn produce(&mut self) {
        for settlement in &mut self.settlements {
            for (&good, &amount) in &settlement.production {
                let scale = *settlement.production_scale.get(&good).unwrap_or(&1.0);
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
        if needed <= 0.0 {
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
            let austerity = consumption_multiplier(settlement);
            if austerity < 0.99 {
                self.events.push(Event::Austerity {
                    settlement: settlement.id,
                    multiplier: austerity,
                });
            }

            for (&good, &needed) in &settlement.consumption {
                let effective_need = needed * austerity;
                let available = settlement.stockpiles.entry(good).or_insert(0.0);

                let consumed = effective_need.min(*available);
                *available -= consumed;

                self.events.push(Event::Consumed {
                    settlement: settlement.id,
                    good,
                    amount: consumed,
                });

                if consumed < effective_need {
                    self.events.push(Event::Shortage {
                        settlement: settlement.id,
                        good,
                        unmet: effective_need - consumed,
                    });
                    shortage_updates.push((settlement.id, good, effective_need, consumed));
                }
            }
        }

        for (settlement_id, good, needed, consumed) in shortage_updates {
            self.apply_shortage_effects(settlement_id, good, needed, consumed);
        }
    }

    fn try_debit_settlement(&mut self, settlement_id: SettlementId, amount: f64) -> f64 {
        let floor = -credit_limit(&self.settlements[settlement_id]);
        let available = (self.settlements[settlement_id].money - floor).max(0.0);
        let paid = amount.min(available);
        self.settlements[settlement_id].money -= paid;
        paid
    }

    fn service_debt_interest(&mut self) {
        let debtors: Vec<(SettlementId, f64)> = self
            .settlements
            .iter()
            .filter(|s| s.money < 0.0)
            .map(|s| (s.id, -s.money))
            .collect();

        if debtors.is_empty() {
            return;
        }

        let mut total_interest = 0.0;
        for (debtor_id, debt) in &debtors {
            let interest = debt * DEBT_INTEREST_RATE;
            let paid = self.try_debit_settlement(*debtor_id, interest);
            total_interest += paid;
            if paid > 0.0 {
                self.events.push(Event::DebtInterest {
                    debtor: *debtor_id,
                    amount: paid,
                });
            }
        }

        let creditor_pool: f64 = self
            .settlements
            .iter()
            .filter(|s| s.money > 0.0)
            .map(|s| s.money)
            .sum();

        if creditor_pool <= 0.0 || total_interest <= 0.0 {
            return;
        }

        for settlement in &mut self.settlements {
            if settlement.money <= 0.0 {
                continue;
            }
            settlement.money += total_interest * settlement.money / creditor_pool;
        }
    }

    fn update_prices(&mut self) {
        let base_prices = base_prices();

        for settlement in &mut self.settlements {
            for &good in &[Good::Food, Good::Ore, Good::Tools] {
                let old_price = *settlement.prices.get(&good).unwrap_or(&base_prices[&good]);

                let stockpile = *settlement.stockpiles.get(&good).unwrap_or(&0.0);
                let monthly_need = *settlement.consumption.get(&good).unwrap_or(&1.0);

                let target_stockpile = monthly_need * STOCK_TARGET_MONTHS;

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

        self.refresh_trader_price_knowledge();
    }

    fn route_neighbors(&self, settlement_id: SettlementId) -> Vec<SettlementId> {
        let mut neighbors = Vec::new();
        for route in &self.routes {
            if route.a == settlement_id {
                neighbors.push(route.b);
            } else if route.b == settlement_id {
                neighbors.push(route.a);
            }
        }
        neighbors
    }

    fn markets_heard_by(&self, location: SettlementId, home_base: SettlementId) -> Vec<SettlementId> {
        let mut heard = vec![location, home_base];
        heard.extend(self.route_neighbors(location));
        heard.extend(self.route_neighbors(home_base));
        heard.sort_unstable();
        heard.dedup();
        heard
    }

    fn rumor_price(&self, trader_id: TraderId, settlement_id: SettlementId, good: Good) -> f64 {
        let actual = self.settlements[settlement_id].prices[&good];
        let good_idx = match good {
            Good::Food => 0,
            Good::Ore => 1,
            Good::Tools => 2,
        };
        let hash = (trader_id * 31 + settlement_id * 17 + good_idx * 7) as f64;
        let noise = ((hash % 100.0) / 100.0 - 0.5) * 2.0 * RUMOR_NOISE;
        actual * (1.0 + noise)
    }

    fn refresh_trader_price_knowledge(&mut self) {
        for trader_id in 0..self.traders.len() {
            let location = self.traders[trader_id].location;
            let home_base = self.traders[trader_id].home_base;
            let heard_from = self.markets_heard_by(location, home_base);

            for settlement_id in heard_from {
                for &good in &[Good::Food, Good::Ore, Good::Tools] {
                    let price = if settlement_id == location || settlement_id == home_base {
                        self.settlements[settlement_id].prices[&good]
                    } else {
                        self.rumor_price(trader_id, settlement_id, good)
                    };
                    self.traders[trader_id]
                        .known_prices
                        .insert((settlement_id, good), price);
                }
            }
        }
    }

    fn sync_trader_prices_at(&mut self, trader_id: TraderId, settlement_id: SettlementId) {
        for &good in &[Good::Food, Good::Ore, Good::Tools] {
            self.traders[trader_id].known_prices.insert(
                (settlement_id, good),
                self.settlements[settlement_id].prices[&good],
            );
        }
    }

    /// Fresh quotes at current location and home; rumors or memory elsewhere.
    fn trader_perceived_price(
        &self,
        trader_id: TraderId,
        settlement_id: SettlementId,
        good: Good,
    ) -> f64 {
        let trader = &self.traders[trader_id];
        let fresh = settlement_id == trader.location || settlement_id == trader.home_base;
        let quote = if fresh {
            self.settlements[settlement_id].prices[&good]
        } else {
            trader
                .known_prices
                .get(&(settlement_id, good))
                .copied()
                .unwrap_or(base_prices()[&good])
        };
        if fresh {
            return quote;
        }
        let bias = (trader.risk_tolerance - 0.5) * 0.12;
        quote * (1.0 + bias)
    }

    fn pay_trader_expenses(&mut self) {
        for trader_id in 0..self.traders.len() {
            let location = self.traders[trader_id].location;
            let home_base = self.traders[trader_id].home_base;

            let upkeep = self.traders[trader_id].trade_capacity * UPKEEP_PER_CAPACITY;
            let upkeep_paid = upkeep.min(self.traders[trader_id].money);
            self.traders[trader_id].money -= upkeep_paid;
            self.settlements[home_base].money += upkeep_paid;
            if upkeep_paid > 0.0 {
                self.events.push(Event::UpkeepPaid {
                    trader: trader_id,
                    home_base,
                    amount: upkeep_paid,
                });
            }

            let capacity_spend = self.traders[trader_id].trade_capacity * LOCAL_SPEND_PER_CAPACITY;
            let spendable = (self.traders[trader_id].money - TRADER_OPERATING_RESERVE).max(0.0);
            let wealth_spend = spendable * TRADER_REPATRIATION_RATE;
            let local_paid = capacity_spend.max(wealth_spend).min(spendable);
            self.traders[trader_id].money -= local_paid;
            self.settlements[location].money += local_paid;
            if local_paid > 0.0 {
                self.events.push(Event::LocalSpending {
                    trader: trader_id,
                    settlement: location,
                    amount: local_paid,
                });
            }
        }
    }

    fn create_trade_shipments(&mut self) {
        let trader_count = self.traders.len();

        for trader_id in 0..trader_count {
            let mut remaining_capacity = self.traders[trader_id].trade_capacity;
            let mut blocked: Vec<(SettlementId, SettlementId, Good)> = Vec::new();

            while remaining_capacity >= 1.0 {
                let Some(opportunity) =
                    self.best_trade_for_trader(trader_id, remaining_capacity, &blocked)
                else {
                    break;
                };

                let seller_id = opportunity.seller;
                let buyer_id = opportunity.buyer;
                let good = opportunity.good;
                let buy_price = self.settlements[seller_id].prices[&good];
                let actual_buyer_price = self.settlements[buyer_id].prices[&good];

                let seller_stock = self.settlements[seller_id]
                    .stockpiles
                    .get(&good)
                    .copied()
                    .unwrap_or(0.0);
                let buyer_spending_power = settlement_purchasing_power(&self.settlements[buyer_id]);

                let affordable_by_buyer = buyer_spending_power / actual_buyer_price;
                let route_capacity_left = self.routes[opportunity.route].capacity_per_month
                    - self.routes[opportunity.route].used_capacity_this_month;

                let quantity = opportunity
                    .suggested_quantity
                    .min(seller_stock)
                    .min(affordable_by_buyer)
                    .min(route_capacity_left)
                    .min(remaining_capacity)
                    .floor();

                if quantity < 1.0 {
                    blocked.push((seller_id, buyer_id, good));
                    continue;
                }

                *self.settlements[seller_id]
                    .stockpiles
                    .entry(good)
                    .or_insert(0.0) -= quantity;
                self.routes[opportunity.route].used_capacity_this_month += quantity;

                let route = &self.routes[opportunity.route];
                let effective_rate =
                    effective_monthly_incident_rate(route, self.traders[trader_id].location);

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
                    expected_sell_price: opportunity.buyer_price,
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
            }
        }
    }

    fn best_trade_for_trader(
        &self,
        trader_id: TraderId,
        max_capacity: f64,
        excluded: &[(SettlementId, SettlementId, Good)],
    ) -> Option<TradeOpportunity> {
        let trader = &self.traders[trader_id];
        let mut best: Option<TradeOpportunity> = None;

        for route in &self.routes {
            let orientations = [(route.a, route.b), (route.b, route.a)];

            for &(seller_id, buyer_id) in &orientations {
                let buyer = &self.settlements[buyer_id];
                let p_eff = effective_monthly_incident_rate(route, trader.location);

                for &good in &[Good::Food, Good::Ore, Good::Tools] {
                    if excluded
                        .iter()
                        .any(|&(s, b, g)| s == seller_id && b == buyer_id && g == good)
                    {
                        continue;
                    }

                    let seller_price = self.trader_perceived_price(trader_id, seller_id, good);
                    let buyer_price = self.trader_perceived_price(trader_id, buyer_id, good);
                    let spread = buyer_price - seller_price - route.cost_per_unit;

                    let monthly_need = buyer.consumption.get(&good).copied().unwrap_or(1.0);
                    let target_stock = monthly_need * STOCK_TARGET_MONTHS;
                    let buyer_stock = *buyer.stockpiles.get(&good).unwrap_or(&0.0);
                    let shortage = (target_stock - buyer_stock).max(0.0);

                    if shortage < 1.0 {
                        continue;
                    }

                    let buyer_urgency = (shortage / monthly_need).clamp(0.0, 2.0).min(1.0);
                    let min_spread =
                        MIN_HEURISTIC_SPREAD * (1.3 - trader.risk_tolerance - 0.3 * buyer_urgency);
                    if spread < min_spread {
                        continue;
                    }

                    let stock = *self.settlements[seller_id]
                        .stockpiles
                        .get(&good)
                        .unwrap_or(&0.0);
                    let suggested_quantity = stock.min(shortage).min(max_capacity).floor();

                    if suggested_quantity < 1.0 {
                        continue;
                    }

                    let cumulative_risk = 1.0 - (1.0 - p_eff).powi(route.travel_months as i32);
                    let risk_discount = 1.0 - cumulative_risk * (1.0 - trader.risk_tolerance);
                    let score =
                        spread * suggested_quantity * risk_discount * (0.4 + 0.6 * buyer_urgency);

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
        let mut rng = rand::thread_rng();

        let pending: Vec<Shipment> = self.shipments.drain(..).collect();

        for mut shipment in pending {
            if shipment.months_remaining > 0 {
                if let Some(lost) = apply_monthly_cargo_risk(
                    &mut shipment.quantity,
                    shipment.effective_monthly_incident_rate,
                    &mut rng,
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
            let route_a = self.routes[shipment.route].a;
            let route_b = self.routes[shipment.route].b;
            let transport_per_unit = self.routes[shipment.route].cost_per_unit;

            let revenue = shipment.quantity * sell_price;
            let purchase_cost = shipment.initial_quantity * shipment.buy_price;
            let transport_cost = shipment.quantity * transport_per_unit;

            let raw_profit = revenue - purchase_cost - transport_cost;
            let profit_cap = revenue * MAX_TRADER_MARGIN;
            let profit = raw_profit.min(profit_cap);
            let buyer_rebate = (raw_profit - profit).max(0.0);

            let buyer_id = shipment.buyer;
            let paid = self.try_debit_settlement(buyer_id, revenue);
            let scale = if revenue > 0.0 { paid / revenue } else { 0.0 };
            let delivered = shipment.quantity * scale;
            let scaled_purchase = purchase_cost * scale;
            let scaled_transport = transport_cost * scale;
            let scaled_profit = profit * scale;
            let scaled_rebate = buyer_rebate * scale;

            self.settlements[shipment.buyer]
                .stockpiles
                .entry(shipment.good)
                .and_modify(|q| *q += delivered)
                .or_insert(delivered);

            self.settlements[shipment.buyer].money += scaled_rebate;
            self.settlements[shipment.seller].money += scaled_purchase;
            self.settlements[route_a].money += scaled_transport / 2.0;
            self.settlements[route_b].money += scaled_transport / 2.0;
            self.traders[shipment.trader].money += scaled_profit;

            self.traders[shipment.trader].location = shipment.buyer;
            self.sync_trader_prices_at(shipment.trader, shipment.buyer);

            self.events.push(Event::ShipmentArrived {
                trader: shipment.trader,
                good: shipment.good,
                quantity: delivered,
                initial_quantity: shipment.initial_quantity,
                seller: shipment.seller,
                buyer: shipment.buyer,
                profit: scaled_profit,
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

fn monthly_production_value(settlement: &Settlement) -> f64 {
    let base = base_prices();
    settlement
        .production
        .iter()
        .map(|(good, amount)| amount * base[good])
        .sum()
}

fn credit_limit(settlement: &Settlement) -> f64 {
    monthly_production_value(settlement) * CREDIT_MONTHS_OF_PRODUCTION
}

fn debt_stress(settlement: &Settlement) -> f64 {
    if settlement.money >= 0.0 {
        return 0.0;
    }
    let debt = -settlement.money;
    let limit = credit_limit(settlement);
    if limit <= 0.0 {
        return 1.0;
    }
    (debt / limit).clamp(0.0, 1.0)
}

fn austerity_multiplier(debt_stress: f64) -> f64 {
    1.0 - MAX_AUSTERITY * debt_stress
}

fn consumption_multiplier(settlement: &Settlement) -> f64 {
    let mut multiplier = austerity_multiplier(debt_stress(settlement));
    if settlement.money > PROSPERITY_THRESHOLD {
        let prosperity =
            ((settlement.money - PROSPERITY_THRESHOLD) / PROSPERITY_THRESHOLD).min(1.0) * 0.25;
        multiplier *= 1.0 + prosperity;
    }
    multiplier
}

/// Cash on hand plus remaining borrowing capacity.
fn settlement_purchasing_power(settlement: &Settlement) -> f64 {
    let limit = credit_limit(settlement);
    if settlement.money >= 0.0 {
        settlement.money + limit
    } else {
        (limit + settlement.money).max(0.0)
    }
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
        Event::LocalSpending {
            trader,
            settlement,
            amount,
        } => EventRow {
            kind: "LocalSpend",
            kind_color: Color::Yellow,
            kind_bold: false,
            place: format!(
                "{} → {}",
                world.traders[*trader].name,
                settlement_name(*settlement)
            ),
            good: None,
            details: format!("-{amount:.1}"),
            details_color: Some(Color::Yellow),
        },
        Event::DebtInterest { debtor, amount } => EventRow {
            kind: "Interest",
            kind_color: Color::Red,
            kind_bold: false,
            place: settlement_name(*debtor),
            good: None,
            details: format!("-{amount:.1}"),
            details_color: Some(Color::Red),
        },
        Event::Austerity {
            settlement,
            multiplier,
        } => EventRow {
            kind: "Austerity",
            kind_color: Color::Red,
            kind_bold: false,
            place: settlement_name(*settlement),
            good: None,
            details: format!("{multiplier:.0}% demand"),
            details_color: Some(Color::Red),
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
        header_cell("Risk tol"),
    ]);

    for t in traders {
        table.add_row(vec![
            place_cell(&t.name),
            Cell::new(&settlements[t.location].name).set_alignment(CellAlignment::Left),
            Cell::new(&settlements[t.home_base].name).set_alignment(CellAlignment::Left),
            num_cell(&format!("{:.1}", t.money)).fg(Color::Green),
            num_cell(&format!("{:.0}/mo", t.trade_capacity)),
            num_cell(&format!("{:.2}", t.risk_tolerance)),
        ]);
    }

    println!("{table}");
}

fn main() {
    let (months, quiet) = parse_args();
    let mut world = create_demo_world();
    let mut monthly_shipments = Vec::new();
    let mut monthly_money: Vec<[f64; 3]> = Vec::new();

    if !quiet {
        println!("{}", "Initial state:".bold().cyan());
        print_settlements(&world.settlements);
        println!();
        print_traders(&world.traders, &world.settlements);
        println!();
    }

    for _ in 0..months {
        world.tick_month();

        let events: Vec<Event> = world.events.drain(..).collect();
        let shipments = events
            .iter()
            .filter(|e| matches!(e, Event::ShipmentCreated { .. }))
            .count();
        monthly_shipments.push(shipments as u32);
        monthly_money.push([
            world.settlements[0].money,
            world.settlements[1].money,
            world.settlements[2].money,
        ]);

        if !quiet {
            println!(
                "\n{}",
                format!("═══ Month {} ═══", world.month).bold().cyan()
            );
            print_events(&events, &world);
            println!();
            print_settlements(&world.settlements);
            println!();
            print_traders(&world.traders, &world.settlements);
        }
    }

    if quiet || months > 24 {
        print_long_run_summary(months, &monthly_shipments, &monthly_money, &world);
    }
}

fn parse_args() -> (u32, bool) {
    let mut months = DEFAULT_MONTHS;
    let mut quiet = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--months" | "-m" => {
                months = args
                    .next()
                    .expect("missing value for --months")
                    .parse()
                    .expect("months must be a u32");
            }
            "--quiet" | "-q" => quiet = true,
            "--help" | "-h" => {
                println!("Usage: mundios [--months N] [--quiet]");
                std::process::exit(0);
            }
            _ => {}
        }
    }

    (months, quiet)
}

fn print_long_run_summary(months: u32, shipments: &[u32], money: &[[f64; 3]], world: &World) {
    let names = ["Greenworld", "Ironmoon", "Forge Prime"];
    let avg_shipments = shipments.iter().sum::<u32>() as f64 / months as f64;
    let months_without_trade = shipments.iter().filter(|&&s| s == 0).count();

    println!(
        "\n{}",
        format!("Long-run summary ({months} months)").bold().cyan()
    );
    println!("Avg shipments/month: {avg_shipments:.1}");
    println!("Months without trade: {months_without_trade}/{months}");
    println!();

    for (i, name) in names.iter().enumerate() {
        let values: Vec<f64> = money.iter().map(|m| m[i]).collect();
        let min = values.iter().copied().reduce(f64::min).unwrap_or(0.0);
        let max = values.iter().copied().reduce(f64::max).unwrap_or(0.0);
        let final_m = world.settlements[i].money;
        println!("  {name}: min ${min:.0}, max ${max:.0}, final ${final_m:.0}");
    }

    let trader_total: f64 = world.traders.iter().map(|t| t.money).sum();
    println!("  Traders total: ${trader_total:.0}");
    println!(
        "  Total money: ${:.0} (conserved: ${:.0})",
        total_money(world),
        world.expected_total_money
    );
}

fn create_demo_world() -> World {
    let base = base_prices();

    let farm_world = Settlement {
        id: 0,
        name: "Greenworld".to_string(),
        stockpiles: HashMap::from([(Good::Food, 500.0), (Good::Ore, 20.0), (Good::Tools, 30.0)]),
        prices: base.clone(),
        production: HashMap::from([(Good::Food, 320.0)]),
        consumption: HashMap::from([(Good::Food, 80.0), (Good::Tools, 10.0)]),
        money: 1500.0,
        production_scale: HashMap::new(),
    };

    let mining_world = Settlement {
        id: 1,
        name: "Ironmoon".to_string(),
        stockpiles: HashMap::from([(Good::Food, 120.0), (Good::Ore, 200.0), (Good::Tools, 20.0)]),
        prices: base.clone(),
        production: HashMap::from([(Good::Ore, 120.0)]),
        consumption: HashMap::from([(Good::Food, 100.0), (Good::Tools, 15.0)]),
        money: 1500.0,
        production_scale: HashMap::new(),
    };

    let factory_world = Settlement {
        id: 2,
        name: "Forge Prime".to_string(),
        stockpiles: HashMap::from([(Good::Food, 120.0), (Good::Ore, 80.0), (Good::Tools, 80.0)]),
        prices: base.clone(),
        production: HashMap::from([(Good::Tools, 70.0)]),
        consumption: HashMap::from([(Good::Food, 120.0), (Good::Ore, 90.0)]),
        money: 1500.0,
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

    let settlements = vec![farm_world, mining_world, factory_world];

    let traders = vec![
        Trader {
            id: 0,
            name: "Free Merchants".to_string(),
            location: 0,
            home_base: 0,
            money: 1000.0,
            trade_capacity: 80.0,
            risk_tolerance: 0.55,
            known_prices: HashMap::new(),
        },
        Trader {
            id: 1,
            name: "Ironmoon Haulers".to_string(),
            location: 1,
            home_base: 1,
            money: 1000.0,
            trade_capacity: 80.0,
            risk_tolerance: 0.75,
            known_prices: HashMap::new(),
        },
    ];

    let mut world = World {
        month: 0,
        expected_total_money: 0.0,
        settlements,
        routes,
        traders,
        shipments: Vec::new(),
        events: Vec::new(),
    };
    world.refresh_trader_price_knowledge();
    world.expected_total_money = total_money(&world);
    world
}
