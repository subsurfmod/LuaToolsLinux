#pragma once

#include "Steam/Types.h"

#include <optional>

namespace PendingAPI {

    void RecordEncryptedTicket(SteamAPICall_t call, AppId_t appID);
    std::optional<AppId_t> TakeEncryptedTicket(SteamAPICall_t call);

} // namespace PendingAPI
