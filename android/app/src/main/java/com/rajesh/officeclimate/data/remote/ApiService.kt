package com.rajesh.officeclimate.data.remote

import com.rajesh.officeclimate.data.model.ApiStatus
import com.rajesh.officeclimate.data.model.AppArtifactMetadata
import com.rajesh.officeclimate.data.model.DailyStatsResponse
import com.rajesh.officeclimate.data.model.LeverageResponse
import com.rajesh.officeclimate.data.model.OHLCResponse
import com.rajesh.officeclimate.data.model.OpeningsResponse
import com.rajesh.officeclimate.data.model.OrchestrationResponse
import com.rajesh.officeclimate.data.model.ProjectLeverageResponse
import com.rajesh.officeclimate.data.model.ProjectFocusResponse
import com.rajesh.officeclimate.data.model.SessionsResponse
import com.rajesh.officeclimate.data.model.TemperatureResponse
import kotlinx.serialization.json.JsonObject
import retrofit2.http.Body
import retrofit2.http.GET
import retrofit2.http.POST
import retrofit2.http.Query

interface ApiService {
    @GET("status")
    suspend fun getStatus(): ApiStatus

    @GET("apps/office-climate/meta.json")
    suspend fun getAppArtifactMetadata(): AppArtifactMetadata

    @POST("erv")
    suspend fun setErvSpeed(@Body body: Map<String, String>): JsonObject

    @POST("hvac")
    suspend fun setHvacMode(@Body body: JsonObject): JsonObject

    @GET("history/sessions")
    suspend fun getSessions(@Query("days") days: Int = 7): SessionsResponse

    @GET("history/co2-ohlc")
    suspend fun getCO2OHLC(@Query("hours") hours: Int = 24): OHLCResponse

    @GET("history/daily-stats")
    suspend fun getDailyStats(@Query("days") days: Int = 7): DailyStatsResponse

    @GET("history/temperature")
    suspend fun getTemperature(@Query("hours") hours: Int = 24): TemperatureResponse

    @GET("history/openings")
    suspend fun getOpenings(@Query("days") days: Int = 7): OpeningsResponse

    @GET("history/orchestration")
    suspend fun getOrchestration(@Query("days") days: Int = 7): OrchestrationResponse

    @GET("history/project-focus")
    suspend fun getProjectFocus(@Query("days") days: Int = 7): ProjectFocusResponse

    @GET("history/leverage")
    suspend fun getLeverage(@Query("days") days: Int = 7): LeverageResponse

    @GET("history/project-leverage")
    suspend fun getProjectLeverage(@Query("days") days: Int = 7): ProjectLeverageResponse
}
