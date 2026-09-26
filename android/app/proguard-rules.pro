# What R8 must leave alone: the ways uniffi's bindings reach the Rust core through JNA, and JNA
# reaches back into Java, by name rather than by a call R8 can see. JNA's AAR ships no rules of its
# own. Each `external fun uniffi_*` is bound to the symbol spelled the same way, which the default
# file's `native <methods>` rule already covers, and the rest is below.

# JNA itself: its native half looks up its classes, fields and methods by name.
-keep class com.sun.jna.** { *; }
# Its desktop half, which Android does not have and nothing here calls.
-dontwarn java.awt.**

# A structure's fields are found by the names `@Structure.FieldOrder` spells (the default file
# keeps the annotation), and one returned by value is built through its constructor.
-keep class * extends com.sun.jna.Structure { *; }

# A callback is called from native code through its one method, which nothing in Kotlin calls.
-keep class * implements com.sun.jna.Callback { *; }
