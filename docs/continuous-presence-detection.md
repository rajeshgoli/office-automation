# Continuous Presence Detection

## Problem

Current presence detection has gaps:

| Method | Limitation |
|--------|------------|
| PIR motion sensor | Only detects movement - sitting still = invisible |
| Mac keyboard/mouse | Watching video, reading, thinking = no signal |
| Door events | Works for transitions, not continuous presence |

Result: System can think you left when you're sitting quietly at your desk.

## Solution: mmWave Radar

mmWave (millimeter-wave) radar detects micro-movements like breathing and heartbeat. Someone sitting completely still at a desk registers as "present."

### Recommended: HiLink LD2410 + ESP32

**Why this combo:**
- Cheap (~$10-15 total)
- Highly configurable (range, sensitivity)
- Integrates via MQTT (same as Qingping)
- No cloud dependency
- ESPHome has native support

**Hardware:**

| Part | Cost | Source |
|------|------|--------|
| HiLink LD2410B | ~$5-8 | AliExpress, Amazon |
| ESP32 DevKit | ~$5-8 | AliExpress, Amazon |
| Dupont wires | ~$1 | Any electronics kit |

The "B" variant has Bluetooth for tuning detection parameters via phone app.

### Wiring

```
LD2410          ESP32
───────         ─────
VCC     ───────  3.3V
GND     ───────  GND
TX      ───────  GPIO16 (RX)
```

### ESPHome Configuration

```yaml
esphome:
  name: office-presence
  platform: ESP32
  board: esp32dev

wifi:
  ssid: !secret wifi_ssid
  password: !secret wifi_password

mqtt:
  broker: !secret mqtt_broker  # Set to your Mac Mini's IP or hostname
  port: 1883
  topic_prefix: presence/office

uart:
  tx_pin: GPIO17
  rx_pin: GPIO16
  baud_rate: 256000

ld2410:

binary_sensor:
  - platform: ld2410
    has_target:
      name: "Presence"
      on_state:
        then:
          - mqtt.publish:
              topic: presence/office/state
              payload: !lambda 'return x ? "present" : "away";'
    has_still_target:
      name: "Still"
    has_moving_target:
      name: "Moving"

sensor:
  - platform: ld2410
    moving_distance:
      name: "Moving Distance"
    still_distance:
      name: "Still Distance"
    detection_distance:
      name: "Detection Distance"
```

### Orchestrator Integration

Add MQTT subscription in orchestrator (similar to Qingping pattern):

```python
# Subscribe to presence topic
await client.subscribe("presence/office/state")

# Handle messages
if topic == "presence/office/state":
    self._mmwave_presence = payload == "present"
```

Use as authoritative presence signal - if mmWave says present, trust it regardless of motion/mac state.

## Alternatives Considered

### Aqara FP2 (~$60)

**Pros:**
- Polished product, no DIY
- Zone detection (define regions)
- Zigbee (needs hub) or Thread

**Cons:**
- 6x the cost
- Needs Zigbee coordinator (you don't have one)
- Less configurable

### Everything Presence One (~$40)

**Pros:**
- Pre-built ESP32 + LD2410 + extras (light, temp, humidity)
- ESPHome pre-flashed
- Nice enclosure

**Cons:**
- 4x the cost of DIY
- Often out of stock

### Chair Pressure Sensor (~$20)

**Pros:**
- Dead simple - you're either in the chair or not
- No false positives

**Cons:**
- Only works for seated presence
- Another thing on your chair

## Recommendation

Start with **LD2410B + ESP32** (~$15). It's cheap enough to experiment with, and if it works well, you have continuous presence detection that doesn't care if you're moving or typing.

Mount it facing your desk area, tune the detection range to cover your chair but not the doorway (to avoid detecting movement outside).

If you want plug-and-play without soldering, the **Everything Presence One** is the turnkey option but costs more and has availability issues.
