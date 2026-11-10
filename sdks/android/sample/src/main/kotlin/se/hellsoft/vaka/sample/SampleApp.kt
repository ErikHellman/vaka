package se.hellsoft.vaka.sample

import android.app.Application
import se.hellsoft.vaka.Vaka
import se.hellsoft.vaka.VakaConfig

class SampleApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Vaka.init(
            this,
            VakaConfig(
                key = BuildConfig.VAKA_KEY,
                endpoint = BuildConfig.VAKA_ENDPOINT,
                release = BuildConfig.VERSION_NAME,
                environment = if (BuildConfig.DEBUG) "debug" else "production",
                debug = BuildConfig.DEBUG,
            ),
        )
    }
}
