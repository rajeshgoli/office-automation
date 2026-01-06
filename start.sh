#!/bin/bash
# Office Climate Automation - Start Script
# Starts backend, frontend, and Mac occupancy detector
# Ctrl+C kills all processes

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Kill any existing processes on our ports
echo "Cleaning up old processes..."
lsof -ti :9001 | xargs kill -9 2>/dev/null || true
lsof -ti :9002 | xargs kill -9 2>/dev/null || true
pkill -f "occupancy_detector.py" 2>/dev/null || true
sleep 1

# Track PIDs for cleanup
BACKEND_PID=""
FRONTEND_PID=""
DETECTOR_PID=""

cleanup() {
    echo ""
    echo "Shutting down..."

    [[ -n "$DETECTOR_PID" ]] && kill $DETECTOR_PID 2>/dev/null && echo "Stopped occupancy detector"
    [[ -n "$FRONTEND_PID" ]] && kill $FRONTEND_PID 2>/dev/null && echo "Stopped frontend"
    [[ -n "$BACKEND_PID" ]] && kill $BACKEND_PID 2>/dev/null && echo "Stopped backend"

    # Kill any remaining processes on our ports
    lsof -ti :9001 | xargs kill -9 2>/dev/null || true
    lsof -ti :9002 | xargs kill -9 2>/dev/null || true

    echo "Done."
    exit 0
}

trap cleanup SIGINT SIGTERM

clear
echo "=== Office Climate Automation ==="
echo ""

# Activate venv
source venv/bin/activate

# Start backend
echo "Starting backend on :9001..."
python run.py --port 9001 &
BACKEND_PID=$!
sleep 2

# Check backend started
if ! kill -0 $BACKEND_PID 2>/dev/null; then
    echo "ERROR: Backend failed to start"
    exit 1
fi
echo "Backend running (PID $BACKEND_PID)"

# Start Mac occupancy detector
echo "Starting Mac occupancy detector..."
python occupancy_detector.py --watch --url http://localhost:9001 &
DETECTOR_PID=$!
echo "Detector running (PID $DETECTOR_PID)"

# Install frontend deps if needed
echo "Checking frontend dependencies..."
npm --prefix frontend install --silent

# Start frontend
echo "Starting frontend on :9002..."
VITE_API_PORT=9001 npm --prefix frontend run dev -- --port 9002 --host &
FRONTEND_PID=$!

echo ""
echo "=== All services running ==="
echo "  Backend:  http://localhost:9001/status"
echo "  Frontend: http://localhost:9002"
echo "  Mobile:   http://$(ipconfig getifaddr en0 2>/dev/null || echo 'YOUR_IP'):9002"
echo ""
echo "Press Ctrl+C to stop all services"
echo ""

# Wait for any process to exit
wait
