package com.pd2launcher.app.ui

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.pd2launcher.app.NetworkCheck
import com.pd2launcher.app.R
import com.pd2launcher.app.SessionService

/** Where the network check stands: running (with the last report, if any), or done with its report. */
sealed interface NetworkState {
    /** The report to show: the last one while a check runs again. */
    val report: NetworkCheck.Report?

    data class Checking(val previous: NetworkCheck.Report? = null) : NetworkState {
        override val report: NetworkCheck.Report? get() = previous
    }

    data class Done(override val report: NetworkCheck.Report) : NetworkState
}

/** The network check as the screens use it: its [state], [onRecheck] to run it again, [onOpenSettings] for the app's settings page. */
class NetworkCheckUi(val onRecheck: () -> Unit, val onOpenSettings: () -> Unit) {
    var state by mutableStateOf<NetworkState?>(null)
}

/** The network check for the screens that show it; null where the app provides none (previews). */
val LocalNetworkCheck = staticCompositionLocalOf<NetworkCheckUi?> { null }

/** The player's words for a check's [result]. */
@StringRes
fun networkText(result: NetworkCheck.Result): Int = when (result) {
    NetworkCheck.Result.OK -> R.string.network_ok
    NetworkCheck.Result.BLOCKED -> R.string.network_blocked
    NetworkCheck.Result.NO_NETWORK -> R.string.network_none
    NetworkCheck.Result.DNS -> R.string.network_dns
    NetworkCheck.Result.BLIZZARD_UNREACHABLE -> R.string.network_blizzard
    NetworkCheck.Result.PD2_UNREACHABLE -> R.string.network_pd2
    NetworkCheck.Result.UNREACHABLE -> R.string.network_unreachable
}

/**
 * Under a Download or Install that failed on the network ([NetworkCheck.NETWORK_FAILURES]): why,
 * from a check run again for that failure, and the app's settings when Android blocks its network.
 * Nothing when the check finds the servers reachable, or has not ended yet.
 */
@Composable
fun NetworkNote(modifier: Modifier = Modifier) {
    val check = LocalNetworkCheck.current ?: return
    val failure = SessionService.status.collectAsState().value.snapshot?.operation?.failure
    if (failure !in NetworkCheck.NETWORK_FAILURES) return
    LaunchedEffect(failure) { check.onRecheck() }
    val result = check.state?.report?.result ?: return
    if (result == NetworkCheck.Result.OK) return
    Column(modifier) {
        Text(stringResource(networkText(result)), style = Type.Hint, color = Palette.Amber, modifier = Modifier.padding(horizontal = 10.dp))
        if (result == NetworkCheck.Result.BLOCKED) TextLink(stringResource(R.string.check_grant_network), check.onOpenSettings)
    }
}
