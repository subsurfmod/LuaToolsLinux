# Contributing to LuaTools Linux

Thank you for your interest in contributing to LuaTools Linux! This document provides guidelines for contributing to the project.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Code Style](#code-style)
- [Testing](#testing)
- [Pull Request Process](#pull-request-process)
- [Issue Reporting](#issue-reporting)

## Code of Conduct

This project adheres to a [Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code. Please report unacceptable behavior to the project maintainers.

## Getting Started

### Prerequisites

- Fedora Linux (or similar distribution)
- Python 3.10+
- Git
- Native Steam installation
- Millennium v3.6+

### Fork and Clone

1. Fork the repository on GitHub
2. Clone your fork locally:
   ```bash
   git clone https://github.com/yourusername/LuaToolsLinux.git
   cd LuaToolsLinux
   ```
3. Add upstream remote:
   ```bash
   git remote add upstream https://github.com/originalowner/LuaToolsLinux.git
   ```

## Development Setup

```bash
# Install dependencies
pip install --user -e ".[dev]"

# Or with pipx
pipx install --editable .[dev]

# Run tests
pytest

# Run the application
luatools

# Run in offscreen mode (for CI/headless)
QT_QPA_PLATFORM=offscreen luatools
```

## Code Style

We use the following tools to maintain code quality:

- **Black** - Code formatting (line length 100)
- **Ruff** - Linting (replaces flake8, isort, pyflakes)
- **MyPy** - Static type checking (strict mode)
- **isort** - Import sorting (handled by Ruff)

### Formatting

```bash
# Format code
black src/

# Lint
ruff check src/

# Auto-fix linting issues
ruff check --fix src/

# Type check
mypy src/
```

### Pre-commit Hooks

Install pre-commit hooks to automatically format and check code:

```bash
pip install pre-commit
pre-commit install
```

## Testing

### Running Tests

```bash
# Run all tests
pytest

# Run with coverage
pytest --cov=luatools_gui --cov-report=html

# Run specific test file
pytest tests/test_lua_parser.py

# Run with Qt support
pytest -v --tb=short
```

### Writing Tests

- Place tests in `tests/` directory
- Name test files as `test_*.py`
- Use pytest fixtures for Qt widgets
- Mock external dependencies (Steam, Millennium, network)

Example test structure:

```python
# tests/test_lua_parser.py
import pytest
from luatools_gui.parsers.lua_parser import parse

def test_parse_basic_lua():
    content = "addappid(123456)"
    result = parse(content, 123456)
    assert result is not None
    assert result.base_app_id == 123456
    assert len(result.entries) == 1
```

## Pull Request Process

1. **Create a feature branch** from `main`:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make your changes** following the code style guidelines

3. **Write/update tests** for new functionality

4. **Run all checks**:
   ```bash
   black src/
   ruff check --fix src/
   mypy src/
   pytest
   ```

5. **Commit with conventional commits**:
   ```bash
   git commit -m "feat: add drag-and-drop lua file support"
   git commit -m "fix: fix depot downloader -app argument"
   git commit -m "docs: update README with drag-and-drop instructions"
   ```

6. **Push to your fork**:
   ```bash
   git push origin feature/your-feature-name
   ```

7. **Open a Pull Request** against `main` branch

### PR Requirements

- [ ] All tests pass
- [ ] Code follows style guidelines (Black, Ruff, MyPy)
- [ ] New functionality has tests
- [ ] Documentation updated if needed
- [ ] No merge conflicts with `main`
- [ ] Descriptive PR title and description

## Issue Reporting

### Bug Reports

Use the [bug report template](.github/ISSUE_TEMPLATE/bug_report.md) and include:

- **Environment**: OS, Python version, Qt version
- **Steps to reproduce**: Clear, numbered steps
- **Expected behavior**: What should happen
- **Actual behavior**: What actually happens
- **Logs/Screenshots**: Relevant output

### Feature Requests

Use the [feature request template](.github/ISSUE_TEMPLATE/feature_request.md) and include:

- **Problem**: What problem does this solve?
- **Solution**: Proposed implementation
- **Alternatives**: Other approaches considered
- **Context**: Any relevant background

## Architecture Overview

```
luatools_gui/
├── main.py                 # Entry point
├── models/                 # Data models
│   ├── steam.py           # Steam installation detection
│   └── config.py          # SLSsteam config model
├── parsers/               # File parsers
│   └── lua_parser.py      # Lua file parser (addappid/setManifestid)
├── services/              # Business logic
│   ├── steam_service.py   # Steam detection & lua management
│   ├── slssteam_service.py # SLSsteam config.yaml management
│   ├── depot_downloader.py # DepotDownloaderMod integration
│   ├── depot_info.py      # Steam depot metadata (api.steamcmd.net)
│   ├── lua_installer.py   # Lua file installation & SLSsteam sync
│   ├── fixes.py           # Game fixes database
│   └── external_tools.py  # SteamAutoCrack, Steamless management
├── views/                 # UI views
│   ├── main_window.py     # Main window with menu/navigation
│   ├── games_view.py      # Games table + drag-drop
│   ├── depots_view.py     # Variant switcher, depot table
│   ├── fixes_view.py      # Fixes database UI
│   ├── external_tools_view.py # Tools management
│   └── ...
└── widgets/               # Reusable UI components
    └── drop_zone.py       # Drag-drop zone widget
```

## Key Integration Points

### SLSsteam Sync
When installing a lua file, the following are automatically synced to `~/.config/SLSsteam/config.yaml`:
- `AdditionalApps` - AppID added so SLSsteam recognizes the game
- `FakeAppIds` - Base app + each depot with key (depot 480)
- `AppTokens` - First token found in lua
- `DepotKeys` - All depot decryption keys from lua

### Millennium RPC
The GUI communicates with the LuaTools Millennium plugin via HTTP/RPC on port 38495:
- `GetSettingsConfig`, `GetLauncherPath`
- `StartAddViaLuaTools`, `GetAddViaLuaToolsStatus`
- `CheckForFixes`, `ApplyGameFix`
- And many more...

## Release Process

1. Update version in `pyproject.toml`
2. Update `CHANGELOG.md`
3. Create release tag: `git tag v1.2.3`
4. Push tag: `git push origin v1.2.3`
5. GitHub Actions builds and publishes release

## Questions?

- Open a [discussion](https://github.com/yourusername/LuaToolsLinux/discussions)
- Join the [Discord](https://discord.gg/luatools)
- Check existing [issues](https://github.com/yourusername/LuaToolsLinux/issues)

Thank you for contributing to LuaTools Linux! 🎮