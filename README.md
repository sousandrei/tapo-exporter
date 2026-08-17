# Tapo Prometheus Exporter

Exports T310 temperature and humidity data to Prometheus using a Tapo H100 hub.

## HTTP API

The exporter listens on `0.0.0.0:3000`:

- `GET /metrics` exposes Prometheus metrics.
- `GET /healthz` returns `200 OK` when the process is running.
- Unknown routes return `404 Not Found`.

## Configuration

Set these environment variables:

- `TAPO_USERNAME`
- `TAPO_PASSWORD`
- `TAPO_HUB_IP`
- `RUST_LOG` is optional and defaults to `info`.

Successful `/metrics` requests are logged at most once per hour, with the
window anchored to process boot. Non-200 responses and Tapo polling errors are
logged immediately. Successful Tapo metric updates are summarized once per
minute.
