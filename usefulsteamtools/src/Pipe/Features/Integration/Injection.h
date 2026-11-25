#pragma once

#include "Pipe/PipeTypes.h"

namespace PipeManager::Integration {

    // Injects the architecture-matched helper library at most once per game process.
    void Apply(const PipeContext& ctx);

} // namespace PipeManager::Integration
