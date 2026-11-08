---
name: goal
description: Use this to start a goal when the user asks for one, check it, or mark it complete.
---

Use:

```python
await goal.create("objective", token_budget=None)  # token_budget only if the user gives one
await goal.get()  # the current goal, its budget and what it has used
await goal.complete()
await goal.block("reason")  # only the user or an outside change can lift this
```

The harness keeps you working on the goal until you call `goal.complete()`; saying it is done is not enough.

When not to use: `create` for a task the user did not ask to make a goal; `complete` before the goal is fully done, even when the budget runs out; `block` the first time a blocker appears, or because the work is hard, slow or uncertain.
