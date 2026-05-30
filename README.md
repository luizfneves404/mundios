# Simulations and guidelines for thinking about them (NOT A PROJECT ROADMAP)

I'm not trying to simulate reality.
I'm trying to simulate the causes of the behaviors I care about.

Ask: what behaviors do I want the simulation to produce?
- constraints, delays, locality, feedback loops, external shocks, etc

Ask: if I refine this system, how many other systems become more interesting?

Tips
- Aggregate features until distinctions matter
    - if many costs are only fixed money sinks for someone, then it would be better to have just total_costs. If they each have separate causes and effects, then separating might be better
- model bottlenecks explicitly, abstract away background processes
- represent only decision-making boundaries
    - if some entity don't make independent decisions, don't model them
    - if a collective entity behaves as a collection, this level of abstraction is good
- what quantities should be conserved? think about this carefully
- prefer second-order effects


## Abstraction hierarchy
Layer 1 - aggregate outcome
- total_value += growth_rate

Layer 2 - separate causes
- x_rate, y_rate, z_rate, etc

Layer 3 - Explicit agents and behaviors
- individuals, small groups, actions taken, etc

Choose the simplifications that preserve the dynamics I care about.


## When considering a new detail

1. What behavior do I want?
- example: I want shortages to matter

2. What is the cheapest model that produces that behavior?
- growth_rate *= x_ratio

3. What important behaviors are still missing?
- Refine.

## Signs that I did good

- "That outcome makes sense in retrospect, but I didn't explicitly script it."

# How to evolve the project

- code comments to keep track of ideas
- almost no tests
- a lot of logs, including colored logs and tables 

# Next things to think about

does current risk implementation make traders aware of ruin risk? at least indirectly?

it seems that settlements are running out of money, which causes them to be unable to pay for trade, which in turn causes traders to stop trading at all, since the traders know that they won't be able to sell.

we should also model shortages having effects: food shortage decreases population, raw goods shortage decrease production of the good it depends on, etc.

update prices after every change to the inputs to the price formula itself. this is so that productions and previous trades affect the current price, so that two trades in a row don't piggy back on the same amazing opportunity.

differences between traders success were mainly about uneven information and impossibility of predicting the future perfectly. if we make traders too rational and all knowing, it's boring. at the same time, we don't want traders making stupid decisions, so we need them to be able to tolerate short term small inefficiencies but being able to spot when they are making big foreseeable mistakes.
perhaps location should influence information that they know?
stale prices?
produces many cool emergent behaviors.

less important: remove familiarity factor by location, instead do it by route. the more a trader uses a route, the more familiar it becomes to them. the aspect of familiarity attached to the settlement should be reflected in the trader's knowledge/accuracy of the local prices.