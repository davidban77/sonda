# Network Automation Testing

Your automation runbook says "when InterfaceDown fires, run the remediation playbook." But have
you ever tested that end-to-end? Most teams discover broken automation wiring during a real
outage -- the worst possible time. This guide shows you how to use Sonda to generate synthetic
network alerts and verify that your automation engine (Ansible EDA, Prefect, or StackStorm)
actually triggers the right workflow.

**What you need:**

- The [Alerting Pipeline](alerting-pipeline.md) stack running (Sonda, VictoriaMetrics, vmalert, Alertmanager)
- Familiarity with the [Network Device Telemetry](network-device-telemetry.md) scenarios
- An automation engine installed (Ansible EDA, Prefect, or StackStorm)
- `curl` and `jq` in PATH

!!! tip "Build on what exists"
    This guide picks up where the alerting pipeline's webhook delivery stops. If you haven't
    run through the [Alerting Pipeline](alerting-pipeline.md) guide yet, start there -- it
    covers the Sonda to VictoriaMetrics to Alertmanager chain in detail.

---

## The end-to-end picture

```
sonda CLI       VictoriaMetrics    vmalert        Alertmanager     Automation Engine
 |                 |                 |                |                  |
 |-- push -------->|                 |                |                  |
 |  metrics        |<-- evaluate ----|                |                  |
 |                 |--- alert ------>|                |                  |
 |                 |                 |-- notify ----->|                  |
 |                 |                 |                |-- webhook ------>|
 |                 |                 |                |                  |
 |                 |                 |                |     trigger runbook
```

The first four hops are already covered by the alerting pipeline guide. This guide focuses on
the last hop: receiving the Alertmanager webhook and triggering an automation workflow.

---

## Start the alerting stack

If the stack is not already running, bring it up with the alerting profile:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting up -d
```

Verify all services are healthy:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting ps
```

See the [Alerting Pipeline](alerting-pipeline.md#start-the-stack) guide for the full service
table and troubleshooting.

---

## Alert rules for automation testing

The network device telemetry guide includes alert rules with production-style `for:` durations
(30 seconds to 5 minutes). For automation testing, you want alerts to fire quickly so you get
fast feedback on your wiring.

```yaml title="examples/network-automation-alerts.yaml"
groups:
  - name: network-automation-alerts
    interval: 5s
    rules:
      - alert: InterfaceDown
        expr: interface_oper_state{job="snmp"} == 0
        for: 10s
        labels:
          severity: critical
          automation: "true"
        annotations:
          summary: "Interface {{ $labels.ifName }} is down on {{ $labels.device }}"
          description: >
            {{ $labels.ifAlias }} ({{ $labels.ifName }}) on {{ $labels.device }}
            has been operationally down for more than 10 seconds.
          runbook_url: "https://runbooks.example.com/network/interface-down"

      - alert: BGPSessionDown
        expr: bgp_session_state{job="snmp"} == 0
        for: 10s
        labels:
          severity: critical
          automation: "true"
        annotations:
          summary: "BGP session to {{ $labels.bgp_peer }} is down on {{ $labels.device }}"
          description: >
            BGP session to AS{{ $labels.bgp_asn }} ({{ $labels.bgp_peer }}) on
            {{ $labels.device }} has been down for more than 10 seconds.
          runbook_url: "https://runbooks.example.com/network/bgp-session-down"
```

The `automation: "true"` label lets you route only automation-eligible alerts to your engine,
keeping human-notification routes separate. The short `for: 10s` duration means alerts fire
within 15 seconds (one evaluation interval plus the pending duration).

To use these rules with the Docker Compose stack, mount them into vmalert alongside (or instead
of) the default rules:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting down -v

# Copy automation rules alongside the existing rules
cp examples/network-automation-alerts.yaml \
  examples/alertmanager/network-automation-alerts.yml

docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting up -d
```

!!! info "Why copy the file?"
    The vmalert service mounts `examples/alertmanager/alert-rules.yml` and evaluates
    `--rule=/rules/*.yml`. Placing your file in the same directory makes it available
    to vmalert without modifying `docker-compose-victoriametrics.yml`.

---

## Push metrics that trigger alerts

The existing `network-link-failure.yaml` scenario generates `interface_oper_state` transitions
that trigger InterfaceDown. To also trigger BGPSessionDown, you need a BGP metric.

Use the link failure scenario to generate both interface state and BGP session data. The link
failure file already produces `interface_oper_state` with a 10-second down window per 30-second
cycle -- plenty to trigger the `for: 10s` rule. For BGP, add a quick one-liner:

```bash
# Terminal 1: run the link failure scenario (pushes to VictoriaMetrics)
sonda run --scenario examples/network-link-failure.yaml
```

!!! warning "Sink must target VictoriaMetrics"
    The example scenarios default to `stdout`. To push to VictoriaMetrics, change the sink
    in each scenario entry to `http_push` as shown in the
    [Network Device Telemetry](network-device-telemetry.md#push-to-a-monitoring-backend) guide.
    For a quick single-metric test, create a minimal scenario file with `http_push` and the
    sequence generator, then run it with `sonda metrics --scenario your-file.yaml`.

Verify the alert fired in vmalert:

```bash
curl -s http://localhost:8880/api/v1/alerts \
  | jq '.data.alerts[] | select(.labels.alertname == "InterfaceDown") | {state, labels}'
```

Then confirm Alertmanager received it:

```bash
curl -s http://localhost:9093/api/v2/alerts \
  | jq '.[] | select(.labels.alertname == "InterfaceDown") | .labels'
```

---

## Wire the webhook to your automation engine

Alertmanager delivers alerts as HTTP POST requests with a JSON payload. Your automation engine
needs an endpoint that receives these webhooks and triggers the appropriate workflow.

Here is the Alertmanager webhook payload structure (simplified):

```json title="Alertmanager webhook payload"
{
  "status": "firing",
  "alerts": [
    {
      "status": "firing",
      "labels": {
        "alertname": "InterfaceDown",
        "device": "rtr-core-01",
        "ifName": "GigabitEthernet0/0/0",
        "severity": "critical",
        "automation": "true"
      },
      "annotations": {
        "summary": "Interface GigabitEthernet0/0/0 is down on rtr-core-01",
        "runbook_url": "https://runbooks.example.com/network/interface-down"
      },
      "startsAt": "2026-04-04T12:00:00.000Z",
      "endsAt": "0001-01-01T00:00:00Z"
    }
  ]
}
```

Each automation engine consumes this payload differently. Choose your engine below.

=== "Ansible EDA"

    [Ansible Event-Driven Automation](https://www.ansible.com/products/event-driven-automation)
    uses rulebooks that map event sources to actions. The `alertmanager` event source plugin
    listens for webhook POST requests from Alertmanager.

    **Rulebook:**

    ```yaml title="rulebook-interface-down.yml"
    ---
    - name: Network interface remediation
      hosts: all
      sources:
        - ansible.eda.alertmanager:
            host: 0.0.0.0
            port: 5000
      rules:
        - name: Remediate interface down
          condition: >
            event.alert.labels.alertname == "InterfaceDown"
            and event.alert.status == "firing"
          action:
            run_playbook:
              name: playbooks/remediate-interface.yml
              extra_vars:
                device: "{{ event.alert.labels.device }}"
                interface: "{{ event.alert.labels.ifName }}"
    ```

    **Alertmanager route** (add to your `alertmanager.yml`):

    ```yaml title="alertmanager.yml (automation route)"
    route:
      receiver: webhook  # default
      routes:
        - match:
            automation: "true"
          receiver: ansible-eda

    receivers:
      - name: webhook
        webhook_configs:
          - url: http://webhook-receiver:8080
            send_resolved: true
      - name: ansible-eda
        webhook_configs:
          - url: http://eda-server:5000/endpoint
            send_resolved: true
    ```

    **Run the rulebook:**

    ```bash
    ansible-rulebook --rulebook rulebook-interface-down.yml -i inventory.yml
    ```

    When InterfaceDown fires, EDA receives the webhook, matches the condition, and runs
    `playbooks/remediate-interface.yml` with the device and interface as extra vars.

=== "Prefect"

    [Prefect](https://www.prefect.io/) can receive webhooks through
    [Prefect webhooks](https://docs.prefect.io/latest/automate/events/webhook-triggers/) that
    trigger flow runs. Create a webhook endpoint that maps Alertmanager payloads to Prefect events.

    **Flow definition:**

    ```python title="flows/remediate_interface.py"
    from prefect import flow, get_run_logger

    @flow(name="remediate-interface-down")
    def remediate_interface(device: str, interface: str, alert_status: str):
        logger = get_run_logger()
        logger.info(f"Remediating {interface} on {device} (status: {alert_status})")

        # Your remediation logic here:
        # - SSH to device, check interface state
        # - Attempt bounce if admin-down
        # - Open ticket if hardware failure
        logger.info(f"Remediation complete for {interface} on {device}")
    ```

    **Webhook receiver** (a small FastAPI app that bridges Alertmanager to Prefect):

    ```python title="webhook_receiver.py"
    from fastapi import FastAPI, Request
    from flows.remediate_interface import remediate_interface

    app = FastAPI()

    @app.post("/alertmanager")
    async def handle_alert(request: Request):
        payload = await request.json()
        for alert in payload.get("alerts", []):
            if alert["labels"].get("alertname") == "InterfaceDown":
                remediate_interface(
                    device=alert["labels"]["device"],
                    interface=alert["labels"]["ifName"],
                    alert_status=alert["status"],
                )
        return {"status": "ok"}
    ```

    Point Alertmanager's webhook to `http://prefect-receiver:8000/alertmanager`.

=== "StackStorm"

    [StackStorm](https://stackstorm.com/) uses sensors and rules to map events to actions.
    The `stackstorm-alertmanager` pack provides a webhook sensor for Alertmanager.

    **Rule definition:**

    ```yaml title="rules/remediate_interface_down.yaml"
    ---
    name: remediate_interface_down
    pack: network_automation
    description: "Trigger interface remediation on InterfaceDown alert"
    enabled: true

    trigger:
      type: alertmanager.webhook
      parameters: {}

    criteria:
      trigger.body.alerts[0].labels.alertname:
        type: equals
        pattern: "InterfaceDown"
      trigger.body.alerts[0].status:
        type: equals
        pattern: "firing"

    action:
      ref: network_automation.remediate_interface
      parameters:
        device: "{{ trigger.body.alerts[0].labels.device }}"
        interface: "{{ trigger.body.alerts[0].labels.ifName }}"
    ```

    **Alertmanager route:**

    ```yaml title="alertmanager.yml (StackStorm route)"
    receivers:
      - name: stackstorm
        webhook_configs:
          - url: http://stackstorm:9102/v1/webhooks/alertmanager
            send_resolved: true
    ```

    Register the rule and verify it's active:

    ```bash
    st2 rule create rules/remediate_interface_down.yaml
    st2 rule list --pack=network_automation
    ```

---

## Verify the automation triggers

With the alerting stack running and your automation engine wired up, push metrics and watch the
full chain execute.

### Step-by-step verification

**1. Confirm metrics are flowing:**

```bash
curl -s "http://localhost:8428/api/v1/query?query=interface_oper_state" \
  | jq '.data.result[] | {device: .metric.device, ifName: .metric.ifName, value: .value[1]}'
```

**2. Confirm the alert is firing in vmalert:**

```bash
curl -s http://localhost:8880/api/v1/alerts \
  | jq '.data.alerts[] | select(.labels.alertname == "InterfaceDown") | {state, value}'
```

**3. Confirm Alertmanager received the alert:**

```bash
curl -s http://localhost:9093/api/v2/alerts \
  | jq '.[] | select(.labels.alertname == "InterfaceDown")'
```

**4. Confirm webhook delivery** (check the echo server logs for the payload):

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting logs webhook-receiver --tail 20
```

**5. Confirm your automation engine received the event and ran the workflow.** This step
depends on your engine:

=== "Ansible EDA"

    ```bash
    # Check EDA logs for the triggered playbook
    ansible-rulebook --rulebook rulebook-interface-down.yml -i inventory.yml --verbose
    ```

    Look for log lines showing the condition matched and the playbook executed.

=== "Prefect"

    ```bash
    # Check Prefect flow runs
    prefect flow-run ls --flow-name "remediate-interface-down"
    ```

    Verify the flow run completed successfully with the correct device and interface parameters.

=== "StackStorm"

    ```bash
    # Check StackStorm execution history
    st2 execution list --action=network_automation.remediate_interface
    ```

    Verify the execution completed with `status: succeeded`.

---

## Test flap detection

A single interface-down event is the easy case. The harder scenario is flapping -- an interface
that bounces up and down repeatedly. Your automation needs to handle this without triggering
a remediation storm.

### What flapping looks like

The link failure scenario's 30-second cycle already produces a simple flap pattern: 10 seconds
up, 10 seconds down, 10 seconds up. But real flaps are faster and less predictable.

Here is a rapid-flap sequence that toggles every 2--3 seconds:

```yaml title="Rapid flap sequence (inline in a scenario entry)"
generator:
  type: sequence
  # Rapid flap: toggles every 2-3 seconds over a 20-second window
  values: [1, 1, 1, 1, 1,
           0, 0,
           1, 1, 1,
           0, 0, 0,
           1, 1,
           0, 0,
           1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]
  repeat: true
```

And a slow-flap variant with longer down windows:

```yaml title="Slow flap sequence"
generator:
  type: sequence
  # Slow flap: 15s up, 10s down, 5s up, 10s down, 20s up
  values: [1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,
           0,0,0,0,0,0,0,0,0,0,
           1,1,1,1,1,
           0,0,0,0,0,0,0,0,0,0,
           1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1]
  repeat: true
```

### What to validate

Use these flap patterns to test your automation handles each case correctly:

| Flap pattern | Expected behavior | What to check |
|-------------|-------------------|---------------|
| Single down (10s) with `for: 10s` | Alert fires once, resolves once | Runbook triggers exactly once |
| Rapid flap (2--3s toggles) | Alert may not fire (down duration < `for:`) | Runbook should NOT trigger |
| Slow flap (10s down windows) | Alert fires on each down window | Runbook triggers per event, or is deduplicated |

!!! tip "Tuning `for:` as a flap filter"
    The `for:` duration in your alert rule acts as a debounce. If the interface recovers
    before the `for:` timer expires, the alert never fires. Increase `for:` to filter out
    fast flaps; decrease it to catch brief outages. Test both extremes with Sonda to find
    the right balance.

### Vary the timing

The sequence generator gives you precise control over flap timing. At `rate: 1`, each value
in the sequence is one second. To simulate sub-second flaps, increase the rate:

```yaml
rate: 2          # 2 events/second = each sequence value is 500ms
generator:
  type: sequence
  values: [1, 0, 1, 0, 1, 0, 1, 1, 1, 1]
  repeat: true
```

To simulate flaps with longer intervals, decrease the rate:

```yaml
rate: 0.2        # 1 event every 5 seconds = each sequence value is 5s
generator:
  type: sequence
  values: [1, 0, 0, 1, 1, 1]  # 5s up, 10s down, 15s up
  repeat: true
```

---

## Validate remediation workflows end-to-end

The ultimate test: does the full chain work from synthetic metric to completed remediation?
Here is a checklist for validating your automation workflow against Sonda-generated alerts.

### Test matrix

| Test case | Sonda scenario | Expected alert | Expected automation |
|-----------|---------------|---------------|-------------------|
| Interface down | `network-link-failure.yaml` | InterfaceDown fires | Remediation playbook runs |
| Interface recovers | Let sequence cycle back to 1 | InterfaceDown resolves | Resolution handler runs (if configured) |
| BGP session down | BGP sequence (see [Network Device Telemetry](network-device-telemetry.md#bgp-session-state)) | BGPSessionDown fires | BGP remediation runs |
| Rapid flap | Rapid flap sequence (above) | No alert (below `for:` threshold) | No automation triggers |
| Slow flap | Slow flap sequence (above) | Multiple InterfaceDown alerts | Deduplication or rate limiting works |
| Concurrent failures | Run interface + BGP scenarios together | Both alerts fire | Both workflows run without interference |

### Resolution events

Alertmanager sends a `"status": "resolved"` webhook when an alert clears. Your automation
should handle this -- for example, closing a ticket or logging the recovery.

The link failure scenario naturally produces resolution events: after the 10-second down window,
`interface_oper_state` returns to 1, the alert clears, and Alertmanager delivers the resolved
webhook. Verify your engine processes it:

```bash
# Check webhook logs for resolved status
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting logs webhook-receiver \
  | grep -i resolved
```

### Concurrency testing

Run multiple Sonda scenarios simultaneously to test that your automation handles concurrent
alerts correctly. Create a BGP session scenario file:

```yaml title="bgp-session-down.yaml"
name: bgp_session_state
rate: 1
duration: 120s
generator:
  type: sequence
  # 10s Established, 10s down, 10s Established
  values: [1,1,1,1,1,1,1,1,1,1,
           0,0,0,0,0,0,0,0,0,0,
           1,1,1,1,1,1,1,1,1,1]
  repeat: true
labels:
  device: rtr-core-01
  bgp_peer: "192.168.1.1"
  bgp_asn: "65001"
  job: snmp
encoder:
  type: prometheus_text
sink:
  type: http_push
  url: "http://localhost:8428/api/v1/import/prometheus"
  content_type: "text/plain"
```

Then run both scenarios at the same time:

```bash
# Terminal 1: interface failure
sonda run --scenario examples/network-link-failure.yaml &

# Terminal 2: BGP session down
sonda metrics --scenario bgp-session-down.yaml
```

Both InterfaceDown and BGPSessionDown should fire and trigger their respective workflows
without interfering with each other.

---

## Tear down

When you are done testing, stop the alerting stack:

```bash
docker compose -f examples/docker-compose-victoriametrics.yml \
  --profile alerting down -v
```

If you copied the automation alert rules into `examples/alertmanager/`, clean them up:

```bash
rm -f examples/alertmanager/network-automation-alerts.yml
```

---

## Quick reference

| Task | Command |
|------|---------|
| Start alerting stack | `docker compose -f examples/docker-compose-victoriametrics.yml --profile alerting up -d` |
| Run link failure scenario | `sonda run --scenario examples/network-link-failure.yaml` |
| Check vmalert for InterfaceDown | `curl -s http://localhost:8880/api/v1/alerts \| jq '.data.alerts[]'` |
| Check Alertmanager alerts | `curl -s http://localhost:9093/api/v2/alerts \| jq '.[].labels'` |
| Check webhook delivery | `docker compose -f examples/docker-compose-victoriametrics.yml --profile alerting logs webhook-receiver` |
| Tear down | `docker compose -f examples/docker-compose-victoriametrics.yml --profile alerting down -v` |

**Related pages:**

- [Network Device Telemetry](network-device-telemetry.md) -- generating interface and BGP metrics with the sequence generator
- [Alerting Pipeline](alerting-pipeline.md) -- the full Sonda to VictoriaMetrics to Alertmanager pipeline
- [Alert Testing](alert-testing.md) -- generator patterns for testing alert thresholds
- [CI Alert Validation](ci-alert-validation.md) -- automating alert rule validation in GitHub Actions
