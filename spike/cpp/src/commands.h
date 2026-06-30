// Command dispatch, equivalent to spike/rust/src/commands.rs.
#pragma once

#include <string>
#include <vector>

#include "resp.h"
#include "storage.h"

namespace gamestore {

// Dispatches one command. Sets *close to true if the connection should close
// (QUIT). Returns the reply to send.
Reply Dispatch(Store* store, const std::vector<std::string>& args, bool* close);

}  // namespace gamestore
