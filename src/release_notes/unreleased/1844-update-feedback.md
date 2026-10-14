fix: An update that fails says why in the update popup, such as "no internet connection", and offers Retry, instead of coming back as a fresh offer.
fix: With no internet connection, Update stops at once instead of falling back to a slow `cargo install` that needs the network too.
feature: Help: Update croft and its chord install the version the popup offers, and with none on offer check for a newer release now and say what they found: up to date, a newer version, or that the network could not be reached.
