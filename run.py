#!/usr/bin/env python3
"""Run the office climate automation orchestrator."""

import argparse
import asyncio
import sys
sys.path.insert(0, ".")

from src.orchestrator import main

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Office Climate Automation")
    parser.add_argument("--port", type=int, default=8080, help="HTTP server port (default: 8080)")
    args = parser.parse_args()

    asyncio.run(main(port=args.port))
