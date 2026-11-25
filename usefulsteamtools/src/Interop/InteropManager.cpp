#include "InteropManager.h"
#include "Interops_CallBack.h"
#include "Interops_Decryption.h"
#include "Interops_IPC.h"
#include "Interops_KeyValues.h"
#include "Interops_Manifest.h"
#include "Interops_Misc.h"
#include "Interops_NetPacket.h"
#include "Interops_Package.h"
#include "Interops_SteamUI.h"
#include "Utils/InteropSupport/VehCommon.h"


namespace SteamUI {

    void CoreHook()   { 
        InteropSteamUI::Install(); 
    }
    void CoreUnhook() { 
        InteropSteamUI::Uninstall(); 
    }
}

namespace SteamClient {

    void CoreHook() {
        InteropCallbacks::Install();
        InteropDecryption::Install();
        InteropIPC::Install();
        // InteropKeyValues::Install();
        InteropManifest::Install();
        InteropMisc::Install();
        InteropNetPacket::Install();
        InteropPackage::Install();
    }

    void CoreUnhook() {
        InteropCallbacks::Uninstall();
        InteropDecryption::Uninstall();
        InteropIPC::Uninstall();
        // InteropKeyValues::Uninstall();
        InteropManifest::Uninstall();
        InteropMisc::Uninstall();
        InteropNetPacket::Uninstall();
        InteropPackage::Uninstall();
        VehCommon::DisarmAll();
        VehCommon::RemoveHandler();
    }
}
