## MODIFIED Requirements

### Requirement: Listing rules

`proxlet ls` SHALL list every rule with its route, its target and the kind of
target (proxy or files), and SHALL mark rules whose host nginx already serves,
with the nginx file and line. On a terminal the output SHALL be an aligned table;
otherwise one rule per line with tab-separated fields and no decoration. With no
rules it SHALL say so and exit zero.

#### Scenario: List rules

- **WHEN** rules exist for `/api` → port 8000 and `/files` → `/home/me/site` and the user runs `proxlet ls`
- **THEN** both rules are shown with route, kind and target

#### Scenario: Rule shadowed by nginx

- **GIVEN** a rule for `site.example/` and nginx config serving `site.example` at `/etc/nginx/sites-enabled/site:3`
- **WHEN** the user runs `proxlet ls | cat`
- **THEN** that rule's line ends with a fourth field `served by nginx (/etc/nginx/sites-enabled/site:3)`, and other rules have an empty fourth field

#### Scenario: Empty list

- **WHEN** no rules exist and the user runs `proxlet ls`
- **THEN** the output says there are no rules and suggests `proxlet add`, and the command exits zero
