# Tapo Prometheus Exporter

Exports T310 temperature and humidity data to Prometheus using a Tapo H100 hub.

## HTTP API

The exporter listens on `0.0.0.0:3000`:

- `GET /metrics` exposes Prometheus metrics.
- `GET /healthz` returns `200 OK` when the process is running.
- Unknown routes return `404 Not Found`.

The exporter publishes `room_temperature` and `room_humidity` gauges, each
labelled with the Tapo device nickname as `name`.

## Configuration

Set these environment variables:

- `TAPO_USERNAME`
- `TAPO_PASSWORD`
- `TAPO_HUB_IP` must be a valid IPv4 or IPv6 address.
- `RUST_LOG` is optional and defaults to `info`.

Configuration is parsed and validated at startup. Missing, empty, or invalid
values produce a clear startup error before the exporter connects to the Tapo
hub. Configuration errors do not include the Tapo password.

Successful `/metrics` requests are logged at most once per hour, with the
window anchored to process boot. Non-200 responses and Tapo polling errors are
logged immediately. Successful Tapo metric updates are summarized once per
minute.
