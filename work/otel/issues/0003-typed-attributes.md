---
status: todo
type: idea
created: 2026-11-01
---
# Typed or promoted attributes

## Problem
Attributes are MAP(VARCHAR, VARCHAR). Promoting frequent semantic-convention attributes (http.route, http.response.status_code, db.system.name) to typed columns, or using VARIANT with shredding, would speed up filters and keep types. See data-model.md section 5.

## Notes
Found during the v0.1 build and review (epic 0001).
