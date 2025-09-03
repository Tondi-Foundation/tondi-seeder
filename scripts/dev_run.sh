#!/bin/bash

# Development Environment Runner for Tondi DNS Seeder
# This script runs tondi_seeder with development-optimized settings

echo "🚀 Starting Tondi DNS Seeder in Development Mode"
echo "================================================"
echo ""

# Check if we're in debug mode
if [ "$1" = "--release" ]; then
    echo "⚠️  Running in RELEASE mode - using production intervals"
    echo "   Use without --release flag for development mode with faster intervals"
    echo ""
    cargo run --release -- --config tondi_seeder.conf
else
    echo "🔧 Development mode enabled with faster intervals:"
    echo "   - New node poll interval: 30 seconds"
    echo "   - Stale timeout: 5 minutes"
    echo "   - Connection retry intervals: 1-10 minutes"
    echo "   - Log level: debug"
    echo ""
    
    # Build in debug mode (this ensures debug_assertions are enabled)
    echo "📦 Building in debug mode..."
    cargo build
    
    if [ $? -eq 0 ]; then
        echo "✅ Build successful, starting with development config..."
        echo ""
        ./target/debug/tondi_seeder --config tondi_seeder.dev.conf
    else
        echo "❌ Build failed!"
        exit 1
    fi
fi
