package io.github.stroblme.accent

import android.app.Application

class AccentApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Native.setUp(this)
    }
}
