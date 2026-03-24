# kotlinx-serialization
-keepattributes *Annotation*, InnerClasses
-dontnote kotlinx.serialization.AnnotationsKt
-keepclassmembers class kotlinx.serialization.json.** { *** Companion; }
-keepclasseswithmembers class kotlinx.serialization.json.** { kotlinx.serialization.KSerializer serializer(...); }
-keep,includedescriptorclasses class com.rajesh.officeclimate.**$$serializer { *; }
-keepclassmembers class com.rajesh.officeclimate.** { *** Companion; }
-keepclasseswithmembers class com.rajesh.officeclimate.** { kotlinx.serialization.KSerializer serializer(...); }
