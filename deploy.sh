#!/bin/bash
# Quick deployment script for JARVIS2026 to Railway

echo "🚀 JARVIS2026 Railway Deployment Script"
echo "========================================"
echo ""

# Check if git is initialized
if [ ! -d .git ]; then
    echo "📦 Initializing Git repository..."
    git init
    git add .
    git commit -m "Initial commit - JARVIS2026 ready for deployment"
    echo "✅ Git repository initialized"
else
    echo "✅ Git repository already exists"
fi

echo ""
echo "Next steps:"
echo "1. Install Railway CLI: npm i -g @railway/cli"
echo "2. Login to Railway: railway login"
echo "3. Initialize project: railway init"
echo "4. Add PostgreSQL: railway add --database postgres"
echo "5. Deploy: railway up"
echo ""
echo "Or deploy via Railway Dashboard:"
echo "👉 https://railway.app/new"
echo ""
echo "See deployment_guide.md for detailed instructions!"
