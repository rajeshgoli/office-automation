package com.rajesh.officeclimate.data.remote

import com.rajesh.officeclimate.data.model.ApiStatus
import kotlinx.serialization.json.JsonObject
import retrofit2.http.Body
import retrofit2.http.GET
import retrofit2.http.POST

interface ApiService {
    @GET("status")
    suspend fun getStatus(): ApiStatus

    @POST("erv")
    suspend fun setErvSpeed(@Body body: Map<String, String>): JsonObject

    @POST("hvac")
    suspend fun setHvacMode(@Body body: JsonObject): JsonObject
}
