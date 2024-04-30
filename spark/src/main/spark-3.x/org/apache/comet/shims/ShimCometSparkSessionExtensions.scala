/*
 * Licensed to the Apache Software Foundation (ASF) under one
 * or more contributor license agreements.  See the NOTICE file
 * distributed with this work for additional information
 * regarding copyright ownership.  The ASF licenses this file
 * to you under the Apache License, Version 2.0 (the
 * "License"); you may not use this file except in compliance
 * with the License.  You may obtain a copy of the License at
 *
 *   http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing,
 * software distributed under the License is distributed on an
 * "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
 * KIND, either express or implied.  See the License for the
 * specific language governing permissions and limitations
 * under the License.
 */

package org.apache.comet.shims

import org.apache.spark.sql.connector.expressions.aggregate.Aggregation
import org.apache.spark.sql.execution.{LimitExec, QueryExecution, SparkPlan}
import org.apache.spark.sql.execution.datasources.v2.parquet.ParquetScan

trait ShimCometSparkSessionExtensions {
  import org.apache.comet.shims.ShimCometSparkSessionExtensions._

  /**
   * TODO: delete after dropping Spark 3.2.0 support and directly call scan.pushedAggregate
   */
  def getPushedAggregate(scan: ParquetScan): Option[Aggregation] = scan.getClass.getDeclaredFields
    .filter(_.getName == "pushedAggregate")
    .map { a => a.setAccessible(true); a }
    .flatMap(_.get(scan).asInstanceOf[Option[Aggregation]])
    .headOption

  /**
   * TODO: delete after dropping Spark 3.2 and 3.3 support
   */
  def getOffset(limit: LimitExec): Int = getOffsetOpt(limit).getOrElse(0)

}

object ShimCometSparkSessionExtensions {
  private def getOffsetOpt(plan: SparkPlan): Option[Int] = plan.getClass.getDeclaredFields
    .filter(_.getName == "offset")
    .map { a => a.setAccessible(true); a.get(plan) }
    .filter(_.isInstanceOf[Int])
    .map(_.asInstanceOf[Int])
    .headOption

  // Extended info is available only since Spark 4.0.0
  // (https://issues.apache.org/jira/browse/SPARK-47289)
  def supportsExtendedExplainInfo(qe: QueryExecution): Boolean = {
    try {
      // Look for QueryExecution.extendedExplainInfo(scala.Function1[String, Unit], SparkPlan)
      qe.getClass.getDeclaredMethod(
        "extendedExplainInfo",
        classOf[String => Unit],
        classOf[SparkPlan])
    } catch {
      case _: NoSuchMethodException | _: SecurityException => return false
    }
    true
  }
}
