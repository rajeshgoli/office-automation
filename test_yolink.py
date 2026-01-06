#!/usr/bin/env python3
"""Test YoLink MQTT subscription for real-time events."""

import asyncio
import json
import time
import sys
sys.path.insert(0, ".")

import aiohttp
import aiomqtt
from src.config import load_config

async def main():
    config = load_config()

    async with aiohttp.ClientSession() as session:
        # Get access token
        print("Authenticating...", flush=True)
        auth_url = "https://api.yosmart.com/open/yolink/token"
        auth_payload = {
            "grant_type": "client_credentials",
            "client_id": config.yolink.uaid,
            "client_secret": config.yolink.secret_key,
        }
        async with session.post(auth_url, json=auth_payload) as resp:
            auth_data = await resp.json()
            access_token = auth_data["access_token"]
            print(f"✓ Got access token", flush=True)

        # Get Home ID
        print("Getting Home ID...", flush=True)
        api_url = "https://api.yosmart.com/open/yolink/v2/api"
        headers = {"Authorization": f"Bearer {access_token}"}
        payload = {"method": "Home.getGeneralInfo", "time": int(time.time() * 1000)}

        async with session.post(api_url, json=payload, headers=headers) as resp:
            home_data = await resp.json()
            home_id = home_data.get("data", {}).get("id")
            if not home_id:
                print(f"Could not get Home ID! Response: {home_data}", flush=True)
                return
            print(f"✓ Home ID: {home_id}", flush=True)

    # Connect to MQTT
    topic = f"yl-home/{home_id}/+/report"
    print(f"\nConnecting to MQTT...", flush=True)
    print(f"Topic: {topic}", flush=True)

    try:
        async with aiomqtt.Client(
            hostname="api.yosmart.com",
            port=8003,
            username=access_token,
            password="",
        ) as client:
            await client.subscribe(topic)
            print("✓ Connected! Waiting 15s for events...\n", flush=True)
            print(">>> Trigger a sensor now! <<<\n", flush=True)

            # Wait for events with timeout
            try:
                async with asyncio.timeout(60):
                    async for message in client.messages:
                        payload = json.loads(message.payload.decode())
                        print(f"EVENT: {payload}", flush=True)
            except asyncio.TimeoutError:
                print("No events received in 60 seconds.", flush=True)

    except Exception as e:
        print(f"MQTT Error: {e}", flush=True)
        import traceback
        traceback.print_exc()

if __name__ == "__main__":
    asyncio.run(main())
